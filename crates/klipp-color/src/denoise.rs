/// Spatial noise reduction using bilateral filter.
///
/// Smooths noise while preserving edges by weighting neighbor pixels
/// based on both spatial proximity and color similarity.
///
/// Parameters:
/// - `rgba`: RGBA pixel buffer (modified in place)
/// - `width`, `height`: frame dimensions
/// - `luma_strength`: noise reduction strength for luminance (0.0 = off, 1.0 = strong)
/// - `chroma_strength`: noise reduction strength for color (0.0 = off, 1.0 = strong)
/// - `radius`: filter radius in pixels (1-7, higher = slower)
pub fn bilateral_denoise(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    luma_strength: f32,
    chroma_strength: f32,
    radius: u32,
) {
    if luma_strength <= 0.0 && chroma_strength <= 0.0 {
        return;
    }
    let w = width as usize;
    let h = height as usize;
    let r = radius.clamp(1, 7) as i32;
    let sigma_s = r as f32 / 2.0;
    let sigma_s2 = 2.0 * sigma_s * sigma_s;
    // Use the stronger of the two strengths for a combined bilateral
    let strength = luma_strength.max(chroma_strength);
    let sigma_r = strength * 40.0 + 1.0; // range sigma
    let sigma_r2 = 2.0 * sigma_r * sigma_r;

    // Pre-compute spatial weights for the kernel
    let kernel_size = (2 * r + 1) as usize;
    let mut spatial_weights = vec![0.0_f32; kernel_size * kernel_size];
    for dy in -r..=r {
        for dx in -r..=r {
            let dist = (dx * dx + dy * dy) as f32;
            let ky = (dy + r) as usize;
            let kx = (dx + r) as usize;
            spatial_weights[ky * kernel_size + kx] = (-dist / sigma_s2).exp();
        }
    }

    let input = rgba.to_vec(); // copy for reading

    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) * 4;
            let cr = input[idx] as f32;
            let cg = input[idx + 1] as f32;
            let cb = input[idx + 2] as f32;

            let mut sum_r = 0.0_f32;
            let mut sum_g = 0.0_f32;
            let mut sum_b = 0.0_f32;
            let mut sum_w = 0.0_f32;

            for dy in -r..=r {
                let ny = y as i32 + dy;
                if ny < 0 || ny >= h as i32 {
                    continue;
                }
                for dx in -r..=r {
                    let nx = x as i32 + dx;
                    if nx < 0 || nx >= w as i32 {
                        continue;
                    }

                    let nidx = (ny as usize * w + nx as usize) * 4;
                    let nr = input[nidx] as f32;
                    let ng = input[nidx + 1] as f32;
                    let nb = input[nidx + 2] as f32;

                    // Spatial weight (pre-computed)
                    let ky = (dy + r) as usize;
                    let kx = (dx + r) as usize;
                    let ws = spatial_weights[ky * kernel_size + kx];

                    // Range/intensity weight
                    let dr = cr - nr;
                    let dg = cg - ng;
                    let db = cb - nb;
                    let color_dist = dr * dr + dg * dg + db * db;
                    let wr = (-color_dist / sigma_r2).exp();

                    let weight = ws * wr;
                    sum_r += nr * weight;
                    sum_g += ng * weight;
                    sum_b += nb * weight;
                    sum_w += weight;
                }
            }

            if sum_w > 0.0 {
                rgba[idx] = (sum_r / sum_w).clamp(0.0, 255.0) as u8;
                rgba[idx + 1] = (sum_g / sum_w).clamp(0.0, 255.0) as u8;
                rgba[idx + 2] = (sum_b / sum_w).clamp(0.0, 255.0) as u8;
                // Alpha unchanged
            }
        }
    }
}
