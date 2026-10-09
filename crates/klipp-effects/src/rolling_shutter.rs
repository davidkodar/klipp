//! Rolling shutter correction — Phase 1: Motion estimation
//!
//! Implements FAST-9 corner detection, SAD patch matching, RANSAC homography
//! estimation, and per-row affine transform computation for rolling shutter
//! correction. All algorithms are implemented from scratch with no external
//! dependencies beyond bytemuck for RowTransform.

use std::collections::{HashMap, VecDeque};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct Corner {
    pub x: u16,
    pub y: u16,
    pub score: u16,
}

pub use crate::homography::{Homography, Match};

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RowTransform {
    pub a: f32,
    pub b: f32,
    pub tx: f32,
    pub c: f32,
    pub d: f32,
    pub ty: f32,
    pub _pad0: f32,
    pub _pad1: f32,
}
// 8 x 4 = 32 bytes, aligned

pub struct MotionCache {
    entries: HashMap<(u64, i64), (Homography, Homography)>,
    order: VecDeque<(u64, i64)>,
    max_entries: usize,
}

// ---------------------------------------------------------------------------
// LCG RNG (no external rand crate)
// ---------------------------------------------------------------------------

pub fn lcg_next(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state
}

// ---------------------------------------------------------------------------
// 2. to_gray_half
// ---------------------------------------------------------------------------

/// Convert RGBA to grayscale at half resolution. Averages 2x2 blocks using
/// BT.709 approximate luma weights.
pub fn to_gray_half(rgba: &[u8], width: u32, height: u32) -> (Vec<u8>, u32, u32) {
    let hw = width / 2;
    let hh = height / 2;
    let mut gray = vec![0u8; (hw * hh) as usize];
    let stride = width as usize * 4;
    for y in 0..hh {
        for x in 0..hw {
            let sx = (x * 2) as usize;
            let sy = (y * 2) as usize;
            let mut sum = 0u32;
            for dy in 0..2usize {
                for dx in 0..2usize {
                    let idx = (sy + dy) * stride + (sx + dx) * 4;
                    let r = rgba[idx] as u32;
                    let g = rgba[idx + 1] as u32;
                    let b = rgba[idx + 2] as u32;
                    sum += (r * 54 + g * 183 + b * 19) >> 8; // BT.709 approx
                }
            }
            gray[(y * hw + x) as usize] = (sum / 4) as u8;
        }
    }
    (gray, hw, hh)
}

// ---------------------------------------------------------------------------
// 3. FAST-9 corner detector
// ---------------------------------------------------------------------------

/// The 16 circle offsets at radius 3 (Bresenham circle).
const FAST_OFFSETS: [(i32, i32); 16] = [
    (0, -3),
    (1, -3),
    (2, -2),
    (3, -1),
    (3, 0),
    (3, 1),
    (2, 2),
    (1, 3),
    (0, 3),
    (-1, 3),
    (-2, 2),
    (-3, 1),
    (-3, 0),
    (-3, -1),
    (-2, -2),
    (-1, -3),
];

/// Cardinal indices (1-based positions 1, 5, 9, 13 mapped to 0-based).
const CARDINAL: [usize; 4] = [0, 4, 8, 12];

/// Detect FAST-9 corners with non-maximum suppression.
pub fn fast9_detect(
    gray: &[u8],
    width: u32,
    height: u32,
    threshold: u8,
    max_corners: usize,
) -> Vec<Corner> {
    let w = width as i32;
    let h = height as i32;
    let t = threshold as i16;

    // First pass: detect corners and compute scores.
    // Store scores in a grid for NMS.
    let mut score_grid = vec![0u16; (width * height) as usize];

    for y in 3..(h - 3) {
        for x in 3..(w - 3) {
            let center = gray[(y * w + x) as usize] as i16;
            let bright_thresh = center + t;
            let dark_thresh = center - t;

            // Quick reject: check 4 cardinal pixels. Need at least 3 to pass.
            let mut bright_count = 0u8;
            let mut dark_count = 0u8;
            for &ci in &CARDINAL {
                let (dx, dy) = FAST_OFFSETS[ci];
                let val = gray[((y + dy) * w + (x + dx)) as usize] as i16;
                if val > bright_thresh {
                    bright_count += 1;
                }
                if val < dark_thresh {
                    dark_count += 1;
                }
            }
            if bright_count < 3 && dark_count < 3 {
                continue;
            }

            // Read all 16 circle pixels.
            let mut circle = [0i16; 16];
            for (i, &(dx, dy)) in FAST_OFFSETS.iter().enumerate() {
                circle[i] = gray[((y + dy) * w + (x + dx)) as usize] as i16;
            }

            // Check for 9 contiguous brighter or darker.
            let mut is_corner = false;
            let mut best_arc_score = 0u32;

            // Brighter arc
            'bright_start: for start in 0..16 {
                let mut run = 0;
                let mut arc_score = 0u32;
                for k in 0..16 {
                    let idx = (start + k) % 16;
                    let diff = circle[idx] - center;
                    if diff > t {
                        run += 1;
                        arc_score += diff as u32;
                        if run >= 9 {
                            is_corner = true;
                            if arc_score > best_arc_score {
                                best_arc_score = arc_score;
                            }
                            continue 'bright_start;
                        }
                    } else {
                        break;
                    }
                }
            }

            // Darker arc
            'dark_start: for start in 0..16 {
                let mut run = 0;
                let mut arc_score = 0u32;
                for k in 0..16 {
                    let idx = (start + k) % 16;
                    let diff = center - circle[idx];
                    if diff > t {
                        run += 1;
                        arc_score += diff as u32;
                        if run >= 9 {
                            is_corner = true;
                            if arc_score > best_arc_score {
                                best_arc_score = arc_score;
                            }
                            continue 'dark_start;
                        }
                    } else {
                        break;
                    }
                }
            }

            if is_corner {
                score_grid[(y * w + x) as usize] = best_arc_score.min(u16::MAX as u32) as u16;
            }
        }
    }

    // Non-maximum suppression (3x3)
    let mut corners = Vec::new();
    for y in 4..(h - 4) {
        for x in 4..(w - 4) {
            let idx = (y * w + x) as usize;
            let s = score_grid[idx];
            if s == 0 {
                continue;
            }
            let mut is_max = true;
            'nms: for dy in -1..=1i32 {
                for dx in -1..=1i32 {
                    if dy == 0 && dx == 0 {
                        continue;
                    }
                    if score_grid[((y + dy) * w + (x + dx)) as usize] > s {
                        is_max = false;
                        break 'nms;
                    }
                }
            }
            if is_max {
                corners.push(Corner {
                    x: x as u16,
                    y: y as u16,
                    score: s,
                });
            }
        }
    }

    // Sort descending by score and truncate.
    corners.sort_unstable_by_key(|c| std::cmp::Reverse(c.score));
    corners.truncate(max_corners);
    corners
}

// ---------------------------------------------------------------------------
// 4. Patch matching (SAD) with cross-check
// ---------------------------------------------------------------------------

/// Sum of Absolute Differences for an (2*half+1) square patch.
#[allow(clippy::too_many_arguments)]
fn sad_patch(
    img_a: &[u8],
    ax: i32,
    ay: i32,
    img_b: &[u8],
    bx: i32,
    by: i32,
    width: i32,
    half: i32,
) -> u32 {
    let mut sum = 0u32;
    for dy in -half..=half {
        let row_a = (ay + dy) * width;
        let row_b = (by + dy) * width;
        for dx in -half..=half {
            let va = img_a[(row_a + ax + dx) as usize] as i32;
            let vb = img_b[(row_b + bx + dx) as usize] as i32;
            sum += (va - vb).unsigned_abs();
        }
    }
    sum
}

/// Match corners between two frames using SAD patches with cross-check.
/// Coordinates in returned Matches are scaled to full resolution (x2).
#[allow(clippy::too_many_arguments)]
pub fn match_patches(
    gray_a: &[u8],
    corners_a: &[Corner],
    gray_b: &[u8],
    corners_b: &[Corner],
    width: u32,
    height: u32,
    patch_half: u32,
    search_radius: u32,
) -> Vec<Match> {
    let w = width as i32;
    let h = height as i32;
    let half = patch_half as i32;
    let radius = search_radius as i32;

    // Filter corners that are far enough from borders for the patch.
    let valid_a: Vec<usize> = (0..corners_a.len())
        .filter(|&i| {
            let c = &corners_a[i];
            c.x as i32 >= half
                && c.y as i32 >= half
                && (c.x as i32) < w - half
                && (c.y as i32) < h - half
        })
        .collect();
    let valid_b: Vec<usize> = (0..corners_b.len())
        .filter(|&i| {
            let c = &corners_b[i];
            c.x as i32 >= half
                && c.y as i32 >= half
                && (c.x as i32) < w - half
                && (c.y as i32) < h - half
        })
        .collect();

    // Forward matches: for each corner in A, find best in B within radius.
    let mut fwd_match: Vec<Option<usize>> = vec![None; corners_a.len()];
    for &ai in &valid_a {
        let ca = &corners_a[ai];
        let mut best_sad = u32::MAX;
        let mut best_bi = None;
        for &bi in &valid_b {
            let cb = &corners_b[bi];
            let dx = (cb.x as i32 - ca.x as i32).abs();
            let dy = (cb.y as i32 - ca.y as i32).abs();
            if dx > radius || dy > radius {
                continue;
            }
            let s = sad_patch(
                gray_a,
                ca.x as i32,
                ca.y as i32,
                gray_b,
                cb.x as i32,
                cb.y as i32,
                w,
                half,
            );
            if s < best_sad {
                best_sad = s;
                best_bi = Some(bi);
            }
        }
        fwd_match[ai] = best_bi;
    }

    // Reverse matches: for each corner in B, find best in A within radius.
    let mut rev_match: Vec<Option<usize>> = vec![None; corners_b.len()];
    for &bi in &valid_b {
        let cb = &corners_b[bi];
        let mut best_sad = u32::MAX;
        let mut best_ai = None;
        for &ai in &valid_a {
            let ca = &corners_a[ai];
            let dx = (ca.x as i32 - cb.x as i32).abs();
            let dy = (ca.y as i32 - cb.y as i32).abs();
            if dx > radius || dy > radius {
                continue;
            }
            let s = sad_patch(
                gray_b,
                cb.x as i32,
                cb.y as i32,
                gray_a,
                ca.x as i32,
                ca.y as i32,
                w,
                half,
            );
            if s < best_sad {
                best_sad = s;
                best_ai = Some(ai);
            }
        }
        rev_match[bi] = best_ai;
    }

    // Cross-check: keep only mutual matches. Scale coords to full res.
    let mut matches = Vec::new();
    for &ai in &valid_a {
        if let Some(bi) = fwd_match[ai] {
            if rev_match[bi] == Some(ai) {
                let ca = &corners_a[ai];
                let cb = &corners_b[bi];
                matches.push(Match {
                    src_x: ca.x as f64 * 2.0,
                    src_y: ca.y as f64 * 2.0,
                    dst_x: cb.x as f64 * 2.0,
                    dst_y: cb.y as f64 * 2.0,
                });
            }
        }
    }
    matches
}

// ---------------------------------------------------------------------------
// 5. RANSAC over the shared homography
// ---------------------------------------------------------------------------

/// RANSAC homography estimation from point matches.
pub fn ransac_homography(
    matches: &[Match],
    iterations: u32,
    inlier_threshold: f64,
    min_inliers: usize,
) -> Option<Homography> {
    if matches.len() < 4 {
        return None;
    }

    let n = matches.len();
    let mut rng_state: u64 = 0xDEAD_BEEF_CAFE_1234;
    let mut best_inlier_count = 0usize;
    let mut best_inliers: Vec<bool> = vec![false; n];

    for _ in 0..iterations {
        // Pick 4 distinct random indices.
        let mut indices = [0usize; 4];
        let mut i = 0;
        while i < 4 {
            let idx = (lcg_next(&mut rng_state) % n as u64) as usize;
            if indices[..i].contains(&idx) {
                continue;
            }
            indices[i] = idx;
            i += 1;
        }

        let src = [
            (matches[indices[0]].src_x, matches[indices[0]].src_y),
            (matches[indices[1]].src_x, matches[indices[1]].src_y),
            (matches[indices[2]].src_x, matches[indices[2]].src_y),
            (matches[indices[3]].src_x, matches[indices[3]].src_y),
        ];
        let dst = [
            (matches[indices[0]].dst_x, matches[indices[0]].dst_y),
            (matches[indices[1]].dst_x, matches[indices[1]].dst_y),
            (matches[indices[2]].dst_x, matches[indices[2]].dst_y),
            (matches[indices[3]].dst_x, matches[indices[3]].dst_y),
        ];

        let h = match Homography::from_4_points(&src, &dst) {
            Some(h) => h,
            None => continue,
        };

        // Count inliers.
        let mut inlier_count = 0;
        let mut inliers = vec![false; n];
        for (j, m) in matches.iter().enumerate() {
            let (px, py) = h.transform(m.src_x, m.src_y);
            let err = ((px - m.dst_x).powi(2) + (py - m.dst_y).powi(2)).sqrt();
            if err < inlier_threshold {
                inliers[j] = true;
                inlier_count += 1;
            }
        }

        if inlier_count > best_inlier_count {
            best_inlier_count = inlier_count;
            best_inliers = inliers;
        }
    }

    if best_inlier_count < min_inliers {
        return None;
    }

    // Refit using all inliers.
    let inlier_matches: Vec<Match> = matches
        .iter()
        .zip(best_inliers.iter())
        .filter_map(|(&m, &is_inlier)| if is_inlier { Some(m) } else { None })
        .collect();

    Homography::from_n_points(&inlier_matches)
}

// ---------------------------------------------------------------------------
// 6. compute_row_transforms
// ---------------------------------------------------------------------------

/// Compute per-row affine transforms for rolling shutter correction.
///
/// `h_prev`: motion from previous frame to current frame.
/// `h_next`: motion from current frame to next frame.
/// `readout_ratio`: fraction of frame time spent reading out the sensor (0.0-1.0).
/// `strength`: correction strength (0.0 = no correction, 1.0 = full).
pub fn compute_row_transforms(
    h_prev: &Homography,
    h_next: &Homography,
    height: u32,
    readout_ratio: f32,
    strength: f32,
) -> Vec<RowTransform> {
    let h = height as f32;
    let mut transforms = Vec::with_capacity(height as usize);

    for r in 0..height {
        let alpha = r as f32 / h;
        let tau = (alpha - 0.5) * readout_ratio;

        let row_h = if tau < 0.0 {
            // Warp backward: interpolate h_prev toward identity by |tau| * strength.
            h_prev.interpolate_toward_identity((-tau * strength) as f64)
        } else {
            // Warp forward: interpolate h_next toward identity, then invert.
            let interp = h_next.interpolate_toward_identity((tau * strength) as f64);
            match interp.inverse() {
                Some(inv) => inv,
                None => Homography::identity(),
            }
        };

        // Extract affine components (drop perspective row).
        transforms.push(RowTransform {
            a: row_h.h[0][0] as f32,
            b: row_h.h[0][1] as f32,
            tx: row_h.h[0][2] as f32,
            c: row_h.h[1][0] as f32,
            d: row_h.h[1][1] as f32,
            ty: row_h.h[1][2] as f32,
            _pad0: 0.0,
            _pad1: 0.0,
        });
    }

    transforms
}

// ---------------------------------------------------------------------------
// 7. MotionCache (LRU)
// ---------------------------------------------------------------------------

impl MotionCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            max_entries,
        }
    }

    pub fn get(&self, clip_id: u64, frame_pts_ms: i64) -> Option<&(Homography, Homography)> {
        self.entries.get(&(clip_id, frame_pts_ms))
    }

    pub fn insert(
        &mut self,
        clip_id: u64,
        frame_pts_ms: i64,
        h_prev: Homography,
        h_next: Homography,
    ) {
        let key = (clip_id, frame_pts_ms);

        if self.entries.contains_key(&key) {
            // Move to back (most recently used).
            self.order.retain(|k| k != &key);
            self.order.push_back(key);
            self.entries.insert(key, (h_prev, h_next));
            return;
        }

        // Evict oldest if at capacity.
        while self.entries.len() >= self.max_entries {
            if let Some(old_key) = self.order.pop_front() {
                self.entries.remove(&old_key);
            }
        }

        self.entries.insert(key, (h_prev, h_next));
        self.order.push_back(key);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

// ---------------------------------------------------------------------------
// 8. Top-level pipeline function
// ---------------------------------------------------------------------------

/// Default parameters for corner detection and matching.
const FAST_THRESHOLD: u8 = 20;
const MAX_CORNERS: usize = 500;
const PATCH_HALF: u32 = 5;
const SEARCH_RADIUS: u32 = 64;
const RANSAC_ITERATIONS: u32 = 500;
const RANSAC_INLIER_THRESHOLD: f64 = 3.0;
const RANSAC_MIN_INLIERS: usize = 10;

/// A whole file's rolling-shutter motion (M4 step 9): for every frame,
/// the homographies to the frame before and after it, as
/// [`estimate_motion`] fits them; identity where a fit fails (or at the
/// file's ends), as the render took a failed estimate.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RollingShutterData {
    pub motion: Vec<(Homography, Homography)>,
    pub fps: f64,
}

/// Analyse a whole file's rolling-shutter motion, `frame_count` frames at
/// `fps`, decoding each once (`decode_frame` takes seconds from the file's
/// start). `progress` is called with 0.0..1.0 and returns false to cancel.
pub fn analyze_clip(
    fps: f64,
    frame_count: usize,
    decode_frame: &mut dyn FnMut(f64) -> Option<(Vec<u8>, u32, u32)>,
    progress: &dyn Fn(f32) -> bool,
) -> Result<RollingShutterData, String> {
    if frame_count < 3 {
        return Err("need at least 3 frames".to_string());
    }
    if fps <= 0.0 {
        return Err("frame rate must be positive".to_string());
    }
    let identity = Homography::identity();
    let mut decode = |f: usize| decode_frame(f as f64 / fps).ok_or_else(|| format!("failed to decode frame {f}"));
    let mut motion = vec![(identity, identity); frame_count];
    let (mut prev, mut curr) = (decode(0)?, decode(1)?);
    for f in 1..frame_count - 1 {
        let next = decode(f + 1)?;
        if let Some(m) = estimate_motion(&prev.0, prev.1, prev.2, &curr.0, curr.1, curr.2, &next.0, next.1, next.2) {
            motion[f] = m;
        }
        prev = std::mem::replace(&mut curr, next);
        if !progress((f + 2) as f32 / frame_count as f32) {
            return Err("cancelled".to_string());
        }
    }
    Ok(RollingShutterData { motion, fps })
}

/// Full motion estimation pipeline for one frame.
/// Returns (H_prev_to_curr, H_curr_to_next) or None if estimation fails.
#[allow(clippy::too_many_arguments)]
pub fn estimate_motion(
    prev_rgba: &[u8],
    prev_w: u32,
    prev_h: u32,
    curr_rgba: &[u8],
    curr_w: u32,
    curr_h: u32,
    next_rgba: &[u8],
    next_w: u32,
    next_h: u32,
) -> Option<(Homography, Homography)> {
    // Convert all 3 frames to half-res grayscale.
    let (gray_prev, pw, ph) = to_gray_half(prev_rgba, prev_w, prev_h);
    let (gray_curr, cw, ch) = to_gray_half(curr_rgba, curr_w, curr_h);
    let (gray_next, nw, nh) = to_gray_half(next_rgba, next_w, next_h);

    // Detect corners in all 3 frames.
    let corners_prev = fast9_detect(&gray_prev, pw, ph, FAST_THRESHOLD, MAX_CORNERS);
    let corners_curr = fast9_detect(&gray_curr, cw, ch, FAST_THRESHOLD, MAX_CORNERS);
    let corners_next = fast9_detect(&gray_next, nw, nh, FAST_THRESHOLD, MAX_CORNERS);

    // Match prev<->curr.
    let matches_prev_curr = match_patches(
        &gray_prev,
        &corners_prev,
        &gray_curr,
        &corners_curr,
        pw.min(cw),
        ph.min(ch),
        PATCH_HALF,
        SEARCH_RADIUS,
    );

    let h_prev = ransac_homography(
        &matches_prev_curr,
        RANSAC_ITERATIONS,
        RANSAC_INLIER_THRESHOLD,
        RANSAC_MIN_INLIERS,
    )?;

    // Match curr<->next.
    let matches_curr_next = match_patches(
        &gray_curr,
        &corners_curr,
        &gray_next,
        &corners_next,
        cw.min(nw),
        ch.min(nh),
        PATCH_HALF,
        SEARCH_RADIUS,
    );

    let h_next = ransac_homography(
        &matches_curr_next,
        RANSAC_ITERATIONS,
        RANSAC_INLIER_THRESHOLD,
        RANSAC_MIN_INLIERS,
    )?;

    Some((h_prev, h_next))
}

// ---------------------------------------------------------------------------
// 9. Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_gray_half() {
        // 4x4 RGBA image, uniform color per pixel for easy verification.
        // Pixel values: R=100, G=150, B=50 everywhere.
        let mut rgba = vec![0u8; 4 * 4 * 4];
        for i in 0..16 {
            rgba[i * 4] = 100; // R
            rgba[i * 4 + 1] = 150; // G
            rgba[i * 4 + 2] = 50; // B
            rgba[i * 4 + 3] = 255; // A
        }
        let (gray, gw, gh) = to_gray_half(&rgba, 4, 4);
        assert_eq!(gw, 2);
        assert_eq!(gh, 2);
        assert_eq!(gray.len(), 4);

        // Expected luma: (100*54 + 150*183 + 50*19) >> 8 = (5400 + 27450 + 950) >> 8
        // = 33800 >> 8 = 131 (integer division)
        // Per-pixel luma: (100*54 + 150*183 + 50*19) >> 8 = 33800 >> 8 = 132
        let expected = ((100u32 * 54 + 150 * 183 + 50 * 19) >> 8) as u8;
        assert_eq!(expected, 132);
        for &v in &gray {
            assert_eq!(v, expected);
        }
    }

    #[test]
    fn test_fast9_uniform() {
        // Uniform gray image should produce no corners.
        let gray = vec![128u8; 100 * 100];
        let corners = fast9_detect(&gray, 100, 100, 20, 500);
        assert!(corners.is_empty());
    }

    #[test]
    fn test_fast9_finds_corners() {
        // Create a 64x64 image with a bright cross at the center (32, 32).
        let mut gray = vec![64u8; 64 * 64];

        // Horizontal line at y=32
        for x in 20..44 {
            gray[32 * 64 + x] = 200;
        }
        // Vertical line at x=32
        for y in 20..44 {
            gray[y * 64 + 32] = 200;
        }

        let corners = fast9_detect(&gray, 64, 64, 30, 100);
        assert!(!corners.is_empty(), "Should detect corners at cross edges");

        // At least some corners should be near the cross area.
        let near_cross = corners
            .iter()
            .any(|c| (c.x as i32 - 32).abs() < 10 && (c.y as i32 - 32).abs() < 10);
        assert!(near_cross, "At least one corner should be near the cross center");
    }

    #[test]
    fn test_homography_identity() {
        let h = Homography::identity();
        let (x, y) = h.transform(10.0, 20.0);
        assert!((x - 10.0).abs() < 1e-9);
        assert!((y - 20.0).abs() < 1e-9);
    }

    #[test]
    fn test_homography_translation() {
        let src = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        let dst = [(5.0, 3.0), (6.0, 3.0), (6.0, 4.0), (5.0, 4.0)];
        let h = Homography::from_4_points(&src, &dst).expect("Should solve translation");

        let (x, y) = h.transform(0.5, 0.5);
        assert!(
            (x - 5.5).abs() < 1e-6,
            "Expected x=5.5, got {x}"
        );
        assert!(
            (y - 3.5).abs() < 1e-6,
            "Expected y=3.5, got {y}"
        );

        // Check another point.
        let (x2, y2) = h.transform(0.0, 0.0);
        assert!((x2 - 5.0).abs() < 1e-6);
        assert!((y2 - 3.0).abs() < 1e-6);
    }

    #[test]
    fn test_ransac_with_outliers() {
        // 20 inlier matches representing a translation of (10, 5).
        let mut matches: Vec<Match> = Vec::new();
        for i in 0..20 {
            let x = (i * 50 + 100) as f64;
            let y = (i * 30 + 100) as f64;
            matches.push(Match {
                src_x: x,
                src_y: y,
                dst_x: x + 10.0,
                dst_y: y + 5.0,
            });
        }

        // 5 outliers with random-ish bad correspondences.
        let mut rng = 42u64;
        for _ in 0..5 {
            let x = (lcg_next(&mut rng) % 1000) as f64;
            let y = (lcg_next(&mut rng) % 1000) as f64;
            matches.push(Match {
                src_x: x,
                src_y: y,
                dst_x: x + 500.0,
                dst_y: y - 300.0,
            });
        }

        let h = ransac_homography(&matches, 500, 3.0, 10)
            .expect("RANSAC should find the translation model");

        // The recovered homography should map (100, 100) to ~(110, 105).
        let (tx, ty) = h.transform(100.0, 100.0);
        assert!(
            (tx - 110.0).abs() < 2.0,
            "Expected tx~110, got {tx}"
        );
        assert!(
            (ty - 105.0).abs() < 2.0,
            "Expected ty~105, got {ty}"
        );
    }

    #[test]
    fn test_row_transforms_identity() {
        let id = Homography::identity();
        let transforms = compute_row_transforms(&id, &id, 100, 0.5, 1.0);
        assert_eq!(transforms.len(), 100);

        for t in &transforms {
            assert!(
                (t.a - 1.0).abs() < 1e-4,
                "a should be ~1.0, got {}",
                t.a
            );
            assert!(t.b.abs() < 1e-4, "b should be ~0, got {}", t.b);
            assert!(t.tx.abs() < 1e-4, "tx should be ~0, got {}", t.tx);
            assert!(t.c.abs() < 1e-4, "c should be ~0, got {}", t.c);
            assert!(
                (t.d - 1.0).abs() < 1e-4,
                "d should be ~1.0, got {}",
                t.d
            );
            assert!(t.ty.abs() < 1e-4, "ty should be ~0, got {}", t.ty);
        }
    }

    #[test]
    fn test_row_transforms_strength_zero() {
        // Even with a non-identity homography, strength=0 should yield identity transforms.
        let src = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        let dst = [(5.0, 3.0), (6.0, 3.0), (6.0, 4.0), (5.0, 4.0)];
        let h = Homography::from_4_points(&src, &dst).unwrap();

        let transforms = compute_row_transforms(&h, &h, 100, 0.5, 0.0);
        assert_eq!(transforms.len(), 100);

        for t in &transforms {
            assert!(
                (t.a - 1.0).abs() < 1e-4,
                "a should be ~1.0 at strength=0, got {}",
                t.a
            );
            assert!(t.b.abs() < 1e-4);
            assert!(t.tx.abs() < 1e-4);
            assert!(t.c.abs() < 1e-4);
            assert!((t.d - 1.0).abs() < 1e-4);
            assert!(t.ty.abs() < 1e-4);
        }
    }

    #[test]
    fn test_motion_cache() {
        let mut cache = MotionCache::new(3);
        let id = Homography::identity();

        // Insert 3 entries.
        cache.insert(1, 100, id, id);
        cache.insert(1, 200, id, id);
        cache.insert(2, 100, id, id);

        assert!(cache.get(1, 100).is_some());
        assert!(cache.get(1, 200).is_some());
        assert!(cache.get(2, 100).is_some());
        assert!(cache.get(99, 99).is_none());

        // Insert a 4th entry; the oldest (1, 100) should be evicted.
        cache.insert(3, 300, id, id);
        assert!(
            cache.get(1, 100).is_none(),
            "Oldest entry should be evicted"
        );
        assert!(cache.get(1, 200).is_some());
        assert!(cache.get(2, 100).is_some());
        assert!(cache.get(3, 300).is_some());

        // Clear.
        cache.clear();
        assert!(cache.get(1, 200).is_none());
        assert!(cache.get(2, 100).is_none());
        assert!(cache.get(3, 300).is_none());
    }
}

#[cfg(test)]
mod analyze_clip_tests {
    use super::*;

    /// One motion pair a frame, the file's ends identity (they have no
    /// frame on one side); fewer than three frames is refused.
    #[test]
    fn a_whole_file_gets_one_motion_pair_a_frame() {
        let mut decode = |t: f64| {
            let n = (t * 24.0).round() as u8;
            let rgba: Vec<u8> = (0..64 * 32).flat_map(|i| {
                let v = (((i % 64) as u8).wrapping_mul(37) ^ ((i / 64) as u8).wrapping_mul(11)).wrapping_add(n);
                [v, v, v, 255]
            }).collect();
            Some((rgba, 64, 32))
        };
        let data = analyze_clip(24.0, 6, &mut decode, &|_| true).unwrap();
        assert_eq!(data.motion.len(), 6);
        let identity = |h: &Homography| h.h == Homography::identity().h;
        assert!(identity(&data.motion[0].0) && identity(&data.motion[0].1), "the first frame");
        assert!(identity(&data.motion[5].0) && identity(&data.motion[5].1), "the last frame");
        assert!(analyze_clip(24.0, 2, &mut decode, &|_| true).is_err());
        assert_eq!(analyze_clip(24.0, 6, &mut decode, &|_| false).unwrap_err(), "cancelled");
    }
}
