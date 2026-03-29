//! Light effects — bloom, glow, and god rays (CPU fallback).
//!
//! These are CPU implementations used when GPU is unavailable.
//! The GPU compute shader versions in gpu.rs are significantly faster.

use crate::blur;

/// Bloom: extract bright pixels above threshold, blur them, blend back.
pub fn apply_bloom(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    threshold: f32,
    radius: f32,
    intensity: f32,
) -> Vec<u8> {
    let len = rgba.len();

    // Step 1: Extract bright areas
    let mut bright = vec![0u8; len];
    for i in (0..len).step_by(4) {
        let r = rgba[i] as f32 / 255.0;
        let g = rgba[i + 1] as f32 / 255.0;
        let b = rgba[i + 2] as f32 / 255.0;
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        if luma > threshold {
            let factor = (luma - threshold) / (1.0 - threshold + 1e-6);
            bright[i] = (r * factor * 255.0).min(255.0) as u8;
            bright[i + 1] = (g * factor * 255.0).min(255.0) as u8;
            bright[i + 2] = (b * factor * 255.0).min(255.0) as u8;
            bright[i + 3] = rgba[i + 3];
        }
    }

    // Step 2: Blur the bright areas
    let blurred = blur::apply(bright, width, height, radius);

    // Step 3: Additive blend back onto original
    let mut output = rgba;
    for i in (0..len).step_by(4) {
        output[i] = (output[i] as f32 + blurred[i] as f32 * intensity).min(255.0) as u8;
        output[i + 1] = (output[i + 1] as f32 + blurred[i + 1] as f32 * intensity).min(255.0) as u8;
        output[i + 2] = (output[i + 2] as f32 + blurred[i + 2] as f32 * intensity).min(255.0) as u8;
    }
    output
}

/// Glow: blur the entire image and blend with original using screen mode.
pub fn apply_glow(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    radius: f32,
    intensity: f32,
) -> Vec<u8> {
    let len = rgba.len();

    // Blur the whole image
    let blurred = blur::apply(rgba.clone(), width, height, radius);

    // Screen blend: result = 1 - (1-a)(1-b)
    let mut output = rgba;
    for i in (0..len).step_by(4) {
        for c in 0..3 {
            let a = output[i + c] as f32 / 255.0;
            let b = blurred[i + c] as f32 / 255.0 * intensity;
            let screen = 1.0 - (1.0 - a) * (1.0 - b);
            output[i + c] = (screen * 255.0).clamp(0.0, 255.0) as u8;
        }
    }
    output
}

/// God rays: radial blur from a light source point.
pub fn apply_light_rays(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    center_x: f32,
    center_y: f32,
    threshold: f32,
    samples: u32,
    decay: f32,
    intensity: f32,
) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let len = rgba.len();
    let cx = center_x * width as f32;
    let cy = center_y * height as f32;
    let samples = samples.clamp(4, 128) as usize;

    // Step 1: Extract bright areas (same as bloom threshold)
    let mut bright = vec![0f32; w * h * 4];
    for i in (0..len).step_by(4) {
        let r = rgba[i] as f32 / 255.0;
        let g = rgba[i + 1] as f32 / 255.0;
        let b = rgba[i + 2] as f32 / 255.0;
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        if luma > threshold {
            bright[i] = r;
            bright[i + 1] = g;
            bright[i + 2] = b;
            bright[i + 3] = 1.0;
        }
    }

    // Step 2: Radial blur from center — march toward the light source
    let mut rays = vec![0f32; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) * 4;
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;

            let mut r_sum = 0.0f32;
            let mut g_sum = 0.0f32;
            let mut b_sum = 0.0f32;
            let mut weight = 1.0f32;
            let step = decay / samples as f32;

            for s in 0..samples {
                let t = s as f32 * step;
                let sx = (x as f32 - dx * t).clamp(0.0, (w - 1) as f32) as usize;
                let sy = (y as f32 - dy * t).clamp(0.0, (h - 1) as f32) as usize;
                let si = (sy * w + sx) * 4;
                r_sum += bright[si] * weight;
                g_sum += bright[si + 1] * weight;
                b_sum += bright[si + 2] * weight;
                weight *= 1.0 - (decay / samples as f32);
            }

            let inv_samples = 1.0 / samples as f32;
            rays[idx] = r_sum * inv_samples;
            rays[idx + 1] = g_sum * inv_samples;
            rays[idx + 2] = b_sum * inv_samples;
        }
    }

    // Step 3: Additive blend rays onto original
    let mut output = rgba;
    for i in (0..len).step_by(4) {
        output[i] = (output[i] as f32 + rays[i] * intensity * 255.0).min(255.0) as u8;
        output[i + 1] = (output[i + 1] as f32 + rays[i + 1] * intensity * 255.0).min(255.0) as u8;
        output[i + 2] = (output[i + 2] as f32 + rays[i + 2] * intensity * 255.0).min(255.0) as u8;
    }
    output
}
