//! Deterministic synthetic frames for perf scenarios. Generated each
//! run — no binary fixtures committed to the repo.

/// Linear-f32 RGBA gradient. R varies on X, G varies on Y, B holds
/// mid-grey, A = 1. Deterministic and covers a wide input range so
/// content-keyed caches (LUTs etc.) behave consistently.
pub fn gradient_frame_linear(width: u32, height: u32) -> Vec<f32> {
    let mut out = vec![0.0f32; (width * height * 4) as usize];
    let w = width as f32 - 1.0;
    let h = height as f32 - 1.0;
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            out[idx] = (x as f32) / w.max(1.0);
            out[idx + 1] = (y as f32) / h.max(1.0);
            out[idx + 2] = 0.5;
            out[idx + 3] = 1.0;
        }
    }
    out
}
