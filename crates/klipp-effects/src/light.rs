//! Light effects — bloom, glow, and god rays (CPU fallback).
//!
//! These are CPU implementations used when GPU is unavailable.
//! The GPU compute shader versions in gpu.rs are significantly faster.

use crate::blur;

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

/// Vignette: darken/lighten edges with configurable shape.
pub fn apply_vignette(
    mut rgba: Vec<u8>, width: u32, height: u32,
    amount: f32, size: f32, softness: f32, roundness: f32,
) -> Vec<u8> {
    let w = width as f32;
    let h = height as f32;
    let cx = w * 0.5;
    let cy = h * 0.5;
    let _aspect = w / h;
    let softness = softness.max(0.01);

    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            // Normalized distance from center
            let nx = (x as f32 - cx) / cx;
            let ny = (y as f32 - cy) / cy;
            // Blend between rectangular and circular distance
            let rect_dist = nx.abs().max(ny.abs());
            let circ_dist = (nx * nx + ny * ny).sqrt();
            let dist = rect_dist * (1.0 - roundness) + circ_dist * roundness;
            // Vignette factor
            let edge = size;
            let falloff = ((dist - edge) / softness).clamp(0.0, 1.0);
            let factor = 1.0 + amount * falloff;
            for c in 0..3 {
                rgba[idx + c] = (rgba[idx + c] as f32 * factor).clamp(0.0, 255.0) as u8;
            }
        }
    }
    rgba
}

/// Diffusion / Pro-Mist: soft highlight bleed for a cinematic dreamy look.
pub fn apply_diffusion(
    rgba: Vec<u8>, width: u32, height: u32,
    amount: f32, radius: f32, threshold: f32, desaturation: f32,
) -> Vec<u8> {
    let len = rgba.len();

    // Extract highlights above threshold
    let mut highlights = vec![0u8; len];
    for i in (0..len).step_by(4) {
        let r = rgba[i] as f32 / 255.0;
        let g = rgba[i + 1] as f32 / 255.0;
        let b = rgba[i + 2] as f32 / 255.0;
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        if luma > threshold {
            let factor = (luma - threshold) / (1.0 - threshold + 1e-6);
            // Desaturate the highlights
            let desat_r = r + (luma - r) * desaturation;
            let desat_g = g + (luma - g) * desaturation;
            let desat_b = b + (luma - b) * desaturation;
            highlights[i] = (desat_r * factor * 255.0).min(255.0) as u8;
            highlights[i + 1] = (desat_g * factor * 255.0).min(255.0) as u8;
            highlights[i + 2] = (desat_b * factor * 255.0).min(255.0) as u8;
            highlights[i + 3] = rgba[i + 3];
        }
    }

    // Blur the highlights
    let blurred = crate::blur::apply(highlights, width, height, radius);

    // Screen blend with amount control
    let mut output = rgba;
    for i in (0..len).step_by(4) {
        for c in 0..3 {
            let a = output[i + c] as f32 / 255.0;
            let b = blurred[i + c] as f32 / 255.0 * amount;
            let screen = 1.0 - (1.0 - a) * (1.0 - b);
            output[i + c] = (screen * 255.0).clamp(0.0, 255.0) as u8;
        }
    }
    output
}

/// Lens Flare: simulated flare disc and ghost elements.
pub fn apply_lens_flare(
    mut rgba: Vec<u8>, width: u32, height: u32,
    pos_x: f32, pos_y: f32, brightness: f32, flare_size: f32,
    ghost_count: u32, ghost_intensity: f32, chromatic_aberration: f32,
) -> Vec<u8> {
    let w = width as f32;
    let h = height as f32;
    let fx = pos_x * w;
    let fy = pos_y * h;
    let cx = w * 0.5;
    let cy = h * 0.5;
    let max_dim = w.max(h);
    let disc_r = flare_size * max_dim;

    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            let px = x as f32;
            let py = y as f32;
            let mut add_r = 0.0f32;
            let mut add_g = 0.0f32;
            let mut add_b = 0.0f32;

            // Main flare disc
            let dist = ((px - fx).powi(2) + (py - fy).powi(2)).sqrt();
            if dist < disc_r {
                let falloff = 1.0 - (dist / disc_r);
                let glow = falloff * falloff * brightness;
                add_r += glow;
                add_g += glow * 0.9;
                add_b += glow * 0.7;
            }

            // Ghost elements along the flare-to-center axis
            for gi in 0..ghost_count {
                let t = (gi as f32 + 1.0) / (ghost_count as f32 + 1.0);
                let ghost_x = fx + (cx - fx) * t * 2.0;
                let ghost_y = fy + (cy - fy) * t * 2.0;
                let ghost_r = disc_r * (0.3 + t * 0.4);
                let gdist = ((px - ghost_x).powi(2) + (py - ghost_y).powi(2)).sqrt();
                if gdist < ghost_r {
                    let falloff = 1.0 - (gdist / ghost_r);
                    let glow = falloff * falloff * ghost_intensity * brightness;
                    // Chromatic aberration: shift R/G/B slightly
                    let ca = chromatic_aberration * t;
                    add_r += glow * (1.0 + ca);
                    add_g += glow;
                    add_b += glow * (1.0 - ca * 0.5);
                }
            }

            rgba[idx] = (rgba[idx] as f32 + add_r * 255.0).min(255.0) as u8;
            rgba[idx + 1] = (rgba[idx + 1] as f32 + add_g * 255.0).min(255.0) as u8;
            rgba[idx + 2] = (rgba[idx + 2] as f32 + add_b * 255.0).min(255.0) as u8;
        }
    }
    rgba
}

/// Light Wrap: bleed bright areas into dark edges (compositing).
pub fn apply_light_wrap(
    rgba: Vec<u8>, width: u32, height: u32,
    wrap_width: f32, intensity: f32, softness: f32,
) -> Vec<u8> {
    let len = rgba.len();
    // Blur to create the light wrap source
    let blurred = crate::blur::apply(rgba.clone(), width, height, wrap_width * softness);

    // Blend: use blurred version where original is dark
    let mut output = rgba;
    for i in (0..len).step_by(4) {
        let luma = (output[i] as f32 * 0.2126 + output[i + 1] as f32 * 0.7152 + output[i + 2] as f32 * 0.0722) / 255.0;
        // More wrap on darker areas
        let wrap_factor = (1.0 - luma) * intensity;
        for c in 0..3 {
            let orig = output[i + c] as f32;
            let blur = blurred[i + c] as f32;
            output[i + c] = (orig + (blur - orig) * wrap_factor).clamp(0.0, 255.0) as u8;
        }
    }
    output
}

/// Anamorphic Streak: horizontal lens flare streak on bright areas.
pub fn apply_anamorphic_streak(
    rgba: Vec<u8>, width: u32, height: u32,
    threshold: f32, length: f32, intensity: f32, tint: f32,
) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let half_len = (length * 0.5) as usize;

    // Extract bright → horizontal blur only
    let mut streak = vec![0.0f32; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) * 4;
            let r = rgba[idx] as f32 / 255.0;
            let g = rgba[idx + 1] as f32 / 255.0;
            let b = rgba[idx + 2] as f32 / 255.0;
            let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            if luma > threshold {
                let factor = (luma - threshold) / (1.0 - threshold + 1e-6);
                let si = (y * w + x) * 3;
                streak[si] = r * factor;
                streak[si + 1] = g * factor;
                streak[si + 2] = b * factor;
            }
        }
    }

    // Horizontal box blur for the streak
    let mut blurred = vec![0.0f32; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let x_start = x.saturating_sub(half_len);
            let x_end = (x + half_len + 1).min(w);
            let count = (x_end - x_start) as f32;
            let mut sr = 0.0f32;
            let mut sg = 0.0f32;
            let mut sb = 0.0f32;
            for sx in x_start..x_end {
                let si = (y * w + sx) * 3;
                sr += streak[si];
                sg += streak[si + 1];
                sb += streak[si + 2];
            }
            let di = (y * w + x) * 3;
            blurred[di] = sr / count;
            blurred[di + 1] = sg / count;
            blurred[di + 2] = sb / count;
        }
    }

    // Apply tint: warm amber (tint=0) to cool blue (tint=1)
    let tint_r = 1.0 + (1.0 - tint) * 0.3;
    let tint_g = 1.0;
    let tint_b = 1.0 + tint * 0.5;

    // Additive blend
    let mut output = rgba;
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) * 4;
            let si = (y * w + x) * 3;
            output[idx] = (output[idx] as f32 + blurred[si] * tint_r * intensity * 255.0).min(255.0) as u8;
            output[idx + 1] = (output[idx + 1] as f32 + blurred[si + 1] * tint_g * intensity * 255.0).min(255.0) as u8;
            output[idx + 2] = (output[idx + 2] as f32 + blurred[si + 2] * tint_b * intensity * 255.0).min(255.0) as u8;
        }
    }
    output
}

/// Film Halation: vintage red/orange bleed around bright highlights.
pub fn apply_film_halation(
    rgba: Vec<u8>, width: u32, height: u32,
    threshold: f32, radius: f32, intensity: f32, hue: f32,
) -> Vec<u8> {
    let len = rgba.len();

    // Halation color based on hue: 0=deep red, 0.5=orange, 1.0=warm yellow
    let hal_r = 1.0f32;
    let hal_g = hue * 0.6;
    let hal_b = hue * hue * 0.2;

    // Extract bright areas
    let mut bright = vec![0u8; len];
    for i in (0..len).step_by(4) {
        let luma = (rgba[i] as f32 * 0.2126 + rgba[i + 1] as f32 * 0.7152 + rgba[i + 2] as f32 * 0.0722) / 255.0;
        if luma > threshold {
            let factor = (luma - threshold) / (1.0 - threshold + 1e-6);
            bright[i] = (hal_r * factor * 255.0).min(255.0) as u8;
            bright[i + 1] = (hal_g * factor * 255.0).min(255.0) as u8;
            bright[i + 2] = (hal_b * factor * 255.0).min(255.0) as u8;
            bright[i + 3] = rgba[i + 3];
        }
    }

    // Blur the halation
    let blurred = crate::blur::apply(bright, width, height, radius);

    // Screen blend
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
