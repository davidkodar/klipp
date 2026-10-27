//! Video stabilization — Phase 1: Trajectory analysis and smoothing.
//!
//! Analyzes frame-to-frame motion by aligning each frame to the one before
//! with the planar tracker held to a similarity (#2754), accumulates
//! absolute trajectory, and applies Gaussian kernel smoothing to produce
//! per-frame correction transforms.

use crate::rolling_shutter::{self, Homography};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Decomposed 2D motion parameters from an affine transform.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct MotionParams {
    pub tx: f64,
    pub ty: f64,
    pub rotation: f64, // radians
    pub scale: f64,
}

/// Raw analysis: per-frame affine transforms for the entire clip, in
/// the frame's own units (#2771): centred on the frame, its height 1, so
/// they hold at any size the frame is rendered at.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StabilizationData {
    /// Frame-to-frame affine transforms. Length = frame_count - 1.
    pub frame_transforms: Vec<Homography>,
    /// Accumulated absolute trajectory. Length = frame_count. `accumulated[0]` = identity.
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
    /// Where each output pixel of frame `i` is taken from in the frame
    /// (`recompose(trajectory[i]) * inverse(recompose(smoothed[i]))`), in
    /// the frame's units: a turn, scale and shift, never a shear.
    pub corrections: Vec<Homography>,
    /// Tracks which smoothing window produced this (for lazy re-smooth detection).
    pub smoothing_window_secs: f64,
    /// RMS frame-to-frame translation magnitude of the raw (unstabilized) trajectory,
    /// in analysis-proxy pixels. Higher = shakier source.
    pub raw_jitter_px: f64,
    /// RMS frame-to-frame translation magnitude of the smoothed trajectory,
    /// in analysis-proxy pixels. Lower = smoother result.
    pub smoothed_jitter_px: f64,
}

impl SmoothedTrajectory {
    /// Percentage by which jitter was reduced (0–100). Returns 0 if raw is zero
    /// (i.e. the source had no shake to remove).
    pub fn jitter_reduction_pct(&self) -> f64 {
        if self.raw_jitter_px < 1e-6 {
            return 0.0;
        }
        let ratio = self.smoothed_jitter_px / self.raw_jitter_px;
        ((1.0 - ratio).max(0.0) * 100.0).min(100.0)
    }
}

/// RMS magnitude of frame-to-frame (tx, ty) deltas across a trajectory.
/// This is the "jitter" metric: high for shaky footage, low for smooth.
fn trajectory_jitter_rms(traj: &[MotionParams]) -> f64 {
    if traj.len() < 2 {
        return 0.0;
    }
    let mut sumsq = 0.0;
    for w in traj.windows(2) {
        let dx = w[1].tx - w[0].tx;
        let dy = w[1].ty - w[0].ty;
        sumsq += dx * dx + dy * dy;
    }
    (sumsq / (traj.len() - 1) as f64).sqrt()
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

/// `values[i]`, carried on past either end as the line through the end
/// and its mirror: a path moving steadily at an end keeps moving, so a
/// pan is followed to the clip's first and last frame (#2771).
fn extended(values: &[f64], i: i64) -> f64 {
    let last = values.len() as i64 - 1;
    let at = |k: i64| values[k.clamp(0, last) as usize];
    if i < 0 {
        2.0 * values[0] - at(-i)
    } else if i > last {
        2.0 * values[last as usize] - at(2 * last - i)
    } else {
        values[i as usize]
    }
}

/// Apply 1D Gaussian smoothing to a signal, its ends carried on by
/// [`extended`].
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
                .map(|(j, &kval)| kval * extended(values, i as i64 + j as i64 - radius as i64))
                .sum()
        })
        .collect();
    result
}

/// How far a frame's smoothed camera may be from its own, in frame
/// heights, radians and scale (#2771).
const MAX_SHIFT: f64 = 0.05;
const MAX_TURN: f64 = 0.03;
const MAX_SCALE: f64 = 0.03;
/// The most a clip is enlarged to fill: past the limits above, never
/// reached.
const MAX_FILL: f64 = 1.5;

/// The frame's units for pixels of a `width` × `height` frame: centred,
/// the height 1.
fn units(width: f64, height: f64) -> Homography {
    let k = 1.0 / height.max(1.0);
    Homography { h: [[k, 0.0, -0.5 * width * k], [0.0, k, -0.5], [0.0, 0.0, 1.0]] }
}

/// `t`, a transform of the frame's units, in pixels of a `width` ×
/// `height` frame.
pub fn in_pixels(t: &Homography, width: f64, height: f64) -> Homography {
    let to = units(width, height);
    let back = to.inverse().unwrap_or_else(Homography::identity);
    back.multiply(t).multiply(&to)
}

impl SmoothedTrajectory {
    /// How much a frame of `aspect` (width over height) must be enlarged
    /// about its centre so that every frame's correction takes its
    /// pixels from inside the frame, never past its edge (#2771): one
    /// zoom for the whole clip, so it doesn't breathe.
    pub fn fill_zoom(&self, aspect: f64) -> f64 {
        let (a, half) = (aspect.max(1e-3), 0.5);
        self.corrections
            .iter()
            .map(|c| {
                let m = &c.h;
                // The corners of the output, half `a` / 2 by half 1 / 2
                // over the zoom, land inside the frame along each axis.
                let across = (m[0][0].abs() * a * half + m[0][1].abs() * half) / (a * half - m[0][2].abs()).max(1e-6);
                let down = (m[1][0].abs() * a * half + m[1][1].abs() * half) / (half - m[1][2].abs()).max(1e-6);
                across.max(down)
            })
            .fold(1.0, f64::max)
            .min(MAX_FILL)
    }
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
        let raw = data.trajectory[i];
        // Never more than a hand's shake from where the camera went: a
        // turn of the shot the window is too long for is followed, not
        // held against (#2771).
        let near = |s: f64, r: f64, by: f64| r + (s - r).clamp(-by, by);
        let sp = MotionParams {
            tx: near(smooth_tx[i], raw.tx, MAX_SHIFT),
            ty: near(smooth_ty[i], raw.ty, MAX_SHIFT),
            rotation: near(smooth_rot[i], raw.rotation, MAX_TURN),
            scale: near(smooth_scale[i], raw.scale, MAX_SCALE * raw.scale),
        };
        smoothed.push(sp);

        // Output pixel to frame pixel: the frame's camera from the
        // smoothed one. Both are a turn, scale and shift, so whatever
        // shear the fits picked up along the way (a tracking shot's
        // parallax) never leans the picture (#2771).
        let correction = match recompose_homography(&sp).inverse() {
            Some(inv) => recompose_homography(&raw).multiply(&inv),
            None => Homography::identity(),
        };
        corrections.push(correction);
    }

    let raw_jitter_px = trajectory_jitter_rms(&data.trajectory);
    let smoothed_jitter_px = trajectory_jitter_rms(&smoothed);

    SmoothedTrajectory {
        smoothed,
        corrections,
        smoothing_window_secs,
        raw_jitter_px,
        smoothed_jitter_px,
    }
}

// ---------------------------------------------------------------------------
// Full clip analysis
// ---------------------------------------------------------------------------

/// Analyze a clip's motion trajectory, on the GPU when there is one.
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
    analyze_clip_on(crate::gpu::planar::PlanarGpu::new().as_mut(), fps, frame_count, decode_frame, progress_callback)
}

/// [`analyze_clip`] on `gpu`, or the CPU without one (#2754): each frame
/// aligned to the one before by the planar tracker over the whole
/// picture, held to a move, turn and size, so what passes through the
/// shot weighs little against the scene it crosses. A frame that can't
/// be aligned is taken as still.
pub fn analyze_clip_on(
    mut gpu: Option<&mut crate::gpu::planar::PlanarGpu>,
    fps: f64,
    frame_count: usize,
    decode_frame: &mut dyn FnMut(f64) -> Option<(Vec<u8>, u32, u32)>,
    progress_callback: &dyn Fn(f32) -> bool,
) -> Result<StabilizationData, String> {
    use crate::planar::{Model, Options, State};
    use crate::window_track::GrayFrame;
    if frame_count < 2 {
        return Err("need at least 2 frames".to_string());
    }
    let grey = |time: f64, f: usize, decode_frame: &mut dyn FnMut(f64) -> Option<(Vec<u8>, u32, u32)>| -> Result<GrayFrame, String> {
        let (rgba, w, h) = decode_frame(time).ok_or_else(|| format!("failed to decode frame {f}"))?;
        let (data, width, height) = rolling_shutter::to_gray_half(&rgba, w, h);
        Ok(GrayFrame { data, width: width as usize, height: height as usize })
    };
    let opts = Options { model: Model::Similarity, lost_after: 1, reacquire_for: 0, ..Options::default() };
    let mut prev = grey(0.0, 0, decode_frame)?;
    let mut frame_transforms = Vec::with_capacity(frame_count - 1);

    for f in 1..frame_count {
        let cur = grey(f as f64 / fps, f, decode_frame)?;
        let (w, h) = (cur.width as f64, cur.height as f64);
        // The whole picture but a tenth round the edge, where what comes
        // into view has nothing to match.
        let quad = [(0.1 * w, 0.1 * h), (0.9 * w, 0.1 * h), (0.9 * w, 0.9 * h), (0.1 * w, 0.9 * h)];
        let mut next = Some(cur.clone());
        let mut give = |_: usize| next.take();
        let tracked = match gpu.as_deref_mut() {
            Some(g) => g.track(&prev, quad, &[], &[f], &mut give, &opts, &|_| true),
            None => crate::planar::track(&prev, quad, &[], &[f], &mut give, &opts, &|_| true),
        };
        // In the frame's units, as it is kept.
        let to = units(w, h);
        let transform = match (tracked.first().filter(|t| t.state != State::Lost), to.inverse()) {
            (Some(t), Some(back)) => to.multiply(&t.h).multiply(&back),
            _ => Homography::identity(),
        };
        frame_transforms.push(transform);

        // Progress callback.
        let progress = f as f32 / (frame_count - 1) as f32;
        if !progress_callback(progress) {
            return Err("cancelled".to_string());
        }
        prev = cur;
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
// Point tracking — track a single point through sequential frames
// ---------------------------------------------------------------------------

/// Match a single patch between two grayscale frames using SAD.
/// Returns the best match position in the current frame, or None if the match
/// quality is too poor (tracking lost).
fn track_point_between_frames(
    prev_gray: &[u8], prev_w: usize, prev_h: usize,
    curr_gray: &[u8], curr_w: usize, curr_h: usize,
    prev_pos: (f64, f64),
    search_radius: u32,
    patch_half: u32,
    loss_threshold: u32,
) -> Option<(f64, f64)> {
    let px = prev_pos.0 as i32;
    let py = prev_pos.1 as i32;
    let ph = patch_half as i32;
    let sr = search_radius as i32;

    let mut best_score = u64::MAX;
    let mut best_x = px;
    let mut best_y = py;

    for dy in -sr..=sr {
        for dx in -sr..=sr {
            let cx = px + dx;
            let cy = py + dy;

            let mut sad = 0u64;
            let mut count = 0u32;
            for ky in -ph..=ph {
                for kx in -ph..=ph {
                    let sx = (px + kx) as usize;
                    let sy = (py + ky) as usize;
                    let dx2 = (cx + kx) as usize;
                    let dy2 = (cy + ky) as usize;
                    if sx < prev_w && sy < prev_h && dx2 < curr_w && dy2 < curr_h {
                        let a = prev_gray[sy * prev_w + sx] as i64;
                        let b = curr_gray[dy2 * curr_w + dx2] as i64;
                        sad += (a - b).unsigned_abs();
                        count += 1;
                    }
                }
            }
            if count > 0 {
                let norm_sad = sad / count as u64;
                if norm_sad < best_score {
                    best_score = norm_sad;
                    best_x = cx;
                    best_y = cy;
                }
            }
        }
    }

    if best_score > loss_threshold as u64 {
        return None;
    }

    Some((best_x as f64, best_y as f64))
}

/// Track a single point through frames.
///
/// - `initial_pos`: (x, y) in pixels at the reference frame (half-res coordinates)
/// - `reference_frame_idx`: frame index where the point was placed
/// - `fps`, `frame_count`: clip properties
/// - `decode_frame`: callback to get frame RGBA at a given time in seconds
/// - `search_radius`: pixel radius for matching search (half-res)
/// - `patch_half`: patch half-size for SAD matching
/// - `loss_threshold`: SAD threshold above which tracking is considered lost
/// - `forward`: true = track forward from reference, false = track backward
/// - `progress`: callback for progress (0.0-1.0), return false to cancel
///
/// Returns Vec of (frame_index, x, y) positions in half-res pixel coordinates.
pub fn track_point(
    initial_pos: (f64, f64),
    reference_frame_idx: usize,
    fps: f64,
    frame_count: usize,
    decode_frame: &mut dyn FnMut(f64) -> Option<(Vec<u8>, u32, u32)>,
    search_radius: u32,
    patch_half: u32,
    loss_threshold: u32,
    forward: bool,
    progress: &dyn Fn(f32) -> bool,
) -> Result<Vec<(usize, f64, f64)>, String> {
    if frame_count == 0 {
        return Ok(Vec::new());
    }

    let mut results = Vec::new();

    // Decode reference frame
    let ref_time = reference_frame_idx as f64 / fps;
    let (ref_rgba, ref_w, ref_h) = decode_frame(ref_time)
        .ok_or_else(|| format!("failed to decode reference frame {reference_frame_idx}"))?;
    let (ref_gray, gw, gh) = rolling_shutter::to_gray_half(&ref_rgba, ref_w, ref_h);

    results.push((reference_frame_idx, initial_pos.0, initial_pos.1));

    let mut prev_gray = ref_gray;
    let mut prev_pos = initial_pos;
    let prev_gw = gw as usize;
    let prev_gh = gh as usize;

    let range: Box<dyn Iterator<Item = usize>> = if forward {
        let start = reference_frame_idx + 1;
        if start >= frame_count {
            return Ok(results);
        }
        Box::new(start..frame_count)
    } else {
        if reference_frame_idx == 0 {
            return Ok(results);
        }
        Box::new((0..reference_frame_idx).rev())
    };

    let total = if forward {
        frame_count.saturating_sub(reference_frame_idx + 1)
    } else {
        reference_frame_idx
    };
    if total == 0 {
        return Ok(results);
    }

    for (step, frame_idx) in range.enumerate() {
        let time = frame_idx as f64 / fps;
        let (rgba, w, h) = decode_frame(time)
            .ok_or_else(|| format!("failed to decode frame {frame_idx}"))?;
        let (gray, cur_gw, cur_gh) = rolling_shutter::to_gray_half(&rgba, w, h);
        let cur_gw = cur_gw as usize;
        let cur_gh = cur_gh as usize;

        match track_point_between_frames(
            &prev_gray, prev_gw, prev_gh,
            &gray, cur_gw, cur_gh,
            prev_pos,
            search_radius,
            patch_half,
            loss_threshold,
        ) {
            Some(new_pos) => {
                results.push((frame_idx, new_pos.0, new_pos.1));
                prev_pos = new_pos;
            }
            None => {
                // Tracking lost — stop
                break;
            }
        }

        prev_gray = gray;

        let p = (step + 1) as f32 / total as f32;
        if !progress(p) {
            return Err("cancelled".to_string());
        }
    }

    Ok(results)
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
            accumulated.iter().map(decompose_homography).collect();

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
            let noise_x = ((rolling_shutter::lcg_next(&mut rng) % 200) as f64 - 100.0) / 20.0; // +/- 5px
            let noise_y = ((rolling_shutter::lcg_next(&mut rng) % 200) as f64 - 100.0) / 20.0;
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
            accumulated.iter().map(decompose_homography).collect();

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
            accumulated.iter().map(decompose_homography).collect();

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
}
