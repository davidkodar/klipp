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

/// sRGB-u8 RGBA gradient with the same geometry as
/// [`gradient_frame_linear`]: R on X, G on Y, B mid-grey, A opaque.
///
/// Exists so the GPU-primed scenario can be handed the shape a hardware
/// decoder actually leaves on the GPU — u8 RGBA, pre-linearisation.
///
/// Note the two fixtures are *comparable in content, not identical in
/// value*: this one is sRGB-encoded, so decoding it yields a
/// perceptually-spaced ramp rather than the linear one. That is fine for
/// a perf comparison, where cost is driven by resolution and node count
/// rather than by the sample values, and it is the honest input for a
/// path whose whole point is that the u8→linear step happens on the GPU.
pub fn gradient_frame_u8(width: u32, height: u32) -> Vec<u8> {
    let mut out = vec![0u8; (width * height * 4) as usize];
    let w = (width as f32 - 1.0).max(1.0);
    let h = (height as f32 - 1.0).max(1.0);
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            out[idx] = (((x as f32) / w) * 255.0).round() as u8;
            out[idx + 1] = (((y as f32) / h) * 255.0).round() as u8;
            out[idx + 2] = 128;
            out[idx + 3] = 255;
        }
    }
    out
}
