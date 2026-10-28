/// Fast Gaussian blur approximation via 3-pass box blur (O(1) per pixel per pass).
/// Uses sliding-window prefix sums — performance is independent of radius.
/// This is the CPU fallback; the GPU path is used when available.
pub fn apply(rgba: Vec<u8>, width: u32, height: u32, radius: f32) -> Vec<u8> {
    if radius < 0.5 {
        return rgba;
    }
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 {
        return rgba;
    }

    // Compute 3 box radii that approximate a Gaussian (Bock's method)
    let boxes = boxes_for_gaussian(radius);

    // Two buffers — ping-pong between them, zero extra allocations
    let mut a = rgba;
    let mut b = vec![0u8; a.len()];

    for &box_r in &boxes {
        box_blur_h(&a, &mut b, w, h, box_r);
        box_blur_v(&b, &mut a, w, h, box_r);
    }

    a
}

/// The widest sigma a blur takes: far past any frame's size.
pub const MAX_SIGMA: f32 = 16_384.0;

/// Compute 3 box sizes that approximate a Gaussian with the given sigma/radius.
/// A sigma that isn't a number is none, and one past `MAX_SIGMA` is held
/// there (#2995): an infinite one made the boxes overflow.
pub fn boxes_for_gaussian(sigma: f32) -> [usize; 3] {
    let sigma = if sigma.is_finite() { sigma.clamp(0.0, MAX_SIGMA) } else { 0.0 };
    let w_ideal = ((12.0 * sigma * sigma / 3.0) + 1.0).sqrt();
    let mut wl = w_ideal.floor() as usize;
    if wl.is_multiple_of(2) {
        wl = wl.saturating_sub(1).max(1);
    }
    let wu = wl + 2;
    let m_ideal = (12.0 * sigma * sigma
        - 3.0 * (wl * wl) as f32
        - 12.0 * wl as f32
        - 9.0)
        / (-4.0 * wl as f32 - 4.0);
    let m = m_ideal.round() as usize;

    let r = |size: usize| (size - 1) / 2;
    [
        r(if 0 < m { wl } else { wu }),
        r(if 1 < m { wl } else { wu }),
        r(if 2 < m { wl } else { wu }),
    ]
}

fn box_blur_h(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    if r == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let diam = r + r + 1;
    let inv = 1.0 / diam as f32;

    for y in 0..h {
        let row = y * w * 4;
        // Seed: left-edge * r + first (r+1) pixels
        let li = row;
        let (lr, lg, lb, la) = (src[li] as u32, src[li+1] as u32, src[li+2] as u32, src[li+3] as u32);
        let mut sr = lr * r as u32;
        let mut sg = lg * r as u32;
        let mut sb = lb * r as u32;
        let mut sa = la * r as u32;

        let init_end = r.min(w - 1);
        for x in 0..=init_end {
            let i = row + x * 4;
            sr += src[i] as u32;
            sg += src[i+1] as u32;
            sb += src[i+2] as u32;
            sa += src[i+3] as u32;
        }
        if r >= w {
            let ri = row + (w - 1) * 4;
            let cnt = (r + 1 - w) as u32;
            sr += src[ri] as u32 * cnt;
            sg += src[ri+1] as u32 * cnt;
            sb += src[ri+2] as u32 * cnt;
            sa += src[ri+3] as u32 * cnt;
        }

        for x in 0..w {
            let oi = row + x * 4;
            dst[oi]   = (sr as f32 * inv + 0.5) as u8;
            dst[oi+1] = (sg as f32 * inv + 0.5) as u8;
            dst[oi+2] = (sb as f32 * inv + 0.5) as u8;
            dst[oi+3] = (sa as f32 * inv + 0.5) as u8;

            // Add right entering pixel
            let ax = (x + r + 1).min(w - 1);
            let ai = row + ax * 4;
            sr += src[ai] as u32;
            sg += src[ai+1] as u32;
            sb += src[ai+2] as u32;
            sa += src[ai+3] as u32;

            // Remove left leaving pixel
            let sx = x.saturating_sub(r);
            let si = row + sx * 4;
            sr -= src[si] as u32;
            sg -= src[si+1] as u32;
            sb -= src[si+2] as u32;
            sa -= src[si+3] as u32;
        }
    }
}

/// Cache-friendly vertical box blur: process in tiles of columns.
fn box_blur_v(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    if r == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let diam = r + r + 1;
    let inv = 1.0 / diam as f32;
    let stride = w * 4;

    // Process columns in tiles for better cache locality
    const TILE: usize = 64;

    let mut x = 0;
    while x < w {
        let x_end = (x + TILE).min(w);
        let tile_w = x_end - x;
        let chans = tile_w * 4;

        // Accumulators for this tile (RGBA interleaved)
        let mut sums = vec![0u32; chans];

        // Seed: top-edge * r + first (r+1) rows
        let top_off = x * 4;
        for c in 0..chans {
            sums[c] = src[top_off + c] as u32 * r as u32;
        }

        let init_end = r.min(h - 1);
        for y in 0..=init_end {
            let off = y * stride + x * 4;
            for c in 0..chans {
                sums[c] += src[off + c] as u32;
            }
        }
        if r >= h {
            let bot_off = (h - 1) * stride + x * 4;
            let cnt = (r + 1 - h) as u32;
            for c in 0..chans {
                sums[c] += src[bot_off + c] as u32 * cnt;
            }
        }

        for y in 0..h {
            let oi = y * stride + x * 4;
            for c in 0..chans {
                dst[oi + c] = (sums[c] as f32 * inv + 0.5) as u8;
            }

            // Add bottom entering row
            let ay = (y + r + 1).min(h - 1);
            let ai = ay * stride + x * 4;
            // Remove top leaving row
            let sy = y.saturating_sub(r);
            let si = sy * stride + x * 4;

            for c in 0..chans {
                sums[c] += src[ai + c] as u32;
                sums[c] -= src[si + c] as u32;
            }
        }

        x = x_end;
    }
}

/// Build a 1D Gaussian kernel (used in tests).
#[cfg(test)]
fn build_kernel(radius: f32) -> Vec<f32> {
    let half = radius.ceil() as i32;
    let sigma = radius / 2.0;
    let sigma2 = 2.0 * sigma * sigma;
    let mut weights = Vec::with_capacity((2 * half + 1) as usize);
    let mut sum = 0.0f32;
    for i in -half..=half {
        let w = (-((i * i) as f32) / sigma2).exp();
        weights.push(w);
        sum += w;
    }
    for w in &mut weights {
        *w /= sum;
    }
    weights
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Any sigma gives boxes, never an overflow (#2995).
    #[test]
    fn any_sigma_gives_boxes() {
        for sigma in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::MAX, -5.0, 0.0, 1e9] {
            let boxes = boxes_for_gaussian(sigma);
            assert!(boxes.iter().all(|&r| r <= MAX_SIGMA as usize * 2), "{sigma}: {boxes:?}");
        }
        assert_eq!(boxes_for_gaussian(f32::INFINITY), boxes_for_gaussian(0.0));
        assert_eq!(boxes_for_gaussian(1e9), boxes_for_gaussian(MAX_SIGMA));
    }

    #[test]
    fn zero_radius_passthrough() {
        let input = vec![100, 150, 200, 255, 50, 60, 70, 128];
        let output = apply(input.clone(), 2, 1, 0.0);
        assert_eq!(input, output);
    }

    #[test]
    fn uniform_image_stays_uniform() {
        let val = 128u8;
        let input = vec![val; 4 * 4 * 4]; // 4x4 solid
        let output = apply(input, 4, 4, 3.0);
        for pixel in output.chunks_exact(4) {
            assert!((pixel[0] as i32 - val as i32).unsigned_abs() <= 1);
            assert!((pixel[1] as i32 - val as i32).unsigned_abs() <= 1);
            assert!((pixel[2] as i32 - val as i32).unsigned_abs() <= 1);
        }
    }

    #[test]
    fn blur_changes_pixels() {
        // 3x1 image: black, white, black
        let input = vec![
            0, 0, 0, 255,
            255, 255, 255, 255,
            0, 0, 0, 255,
        ];
        let output = apply(input, 3, 1, 1.0);
        // Center pixel should be dimmer than 255 (blurred with black neighbours)
        assert!(output[4] < 255);
        // Edge pixels should be brighter than 0 (blurred with white centre)
        assert!(output[0] > 0);
    }

    #[test]
    fn kernel_sums_to_one() {
        let k = build_kernel(5.0);
        let sum: f32 = k.iter().sum();
        assert!((sum - 1.0).abs() < 0.001);
    }

    #[test]
    fn large_radius_doesnt_panic() {
        let input = vec![128u8; 10 * 10 * 4];
        let output = apply(input, 10, 10, 20.0);
        assert_eq!(output.len(), 10 * 10 * 4);
    }

    #[test]
    fn large_image_perf() {
        // 1920x1080 solid — should complete in well under 100ms
        let input = vec![128u8; 1920 * 1080 * 4];
        let t = std::time::Instant::now();
        let output = apply(input, 1920, 1080, 10.0);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(output.len(), 1920 * 1080 * 4);
        // Sanity: should be fast (< 50ms in debug, < 10ms in release)
        assert!(ms < 500.0, "blur took {ms:.1}ms — too slow");
    }
}
