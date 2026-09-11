/// Exposure: multiply linear RGB by 2^stops.
/// stops = 0.0 is identity.
pub fn apply_exposure(rgba: &mut [u8], stops: f32) {
    if stops == 0.0 {
        return;
    }
    let factor = 2.0f32.powf(stops);
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[0] = ((pixel[0] as f32 / 255.0 * factor).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[1] = ((pixel[1] as f32 / 255.0 * factor).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[2] = ((pixel[2] as f32 / 255.0 * factor).clamp(0.0, 1.0) * 255.0) as u8;
    }
}

/// Contrast: S-curve around midpoint 0.5.
/// factor > 0 increases contrast, < 0 decreases. 0.0 is identity.
pub fn apply_contrast(rgba: &mut [u8], factor: f32) {
    if factor == 0.0 {
        return;
    }
    let scale = 1.0 + factor;
    for pixel in rgba.chunks_exact_mut(4) {
        for ch in &mut pixel[..3] {
            let v = *ch as f32 / 255.0 - 0.5;
            let out = (v * scale + 0.5).clamp(0.0, 1.0);
            *ch = (out * 255.0) as u8;
        }
    }
}

/// Saturation: lerp toward BT.709 luma.
/// factor = 1.0 is identity, 0.0 is grayscale, >1.0 is oversaturated.
pub fn apply_saturation(rgba: &mut [u8], factor: f32) {
    if factor == 1.0 {
        return;
    }
    for pixel in rgba.chunks_exact_mut(4) {
        let r = pixel[0] as f32 / 255.0;
        let g = pixel[1] as f32 / 255.0;
        let b = pixel[2] as f32 / 255.0;
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        pixel[0] = ((luma + (r - luma) * factor).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[1] = ((luma + (g - luma) * factor).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[2] = ((luma + (b - luma) * factor).clamp(0.0, 1.0) * 255.0) as u8;
    }
}

/// Vibrance: saturation-weighted boost — muted colors get boosted more,
/// already-saturated colors (especially skin tones) are left alone.
/// amount = 0.0 is identity. Range -1.0 to 1.0.
pub fn apply_vibrance(r: &mut f64, g: &mut f64, b: &mut f64, amount: f64) {
    let max = r.max(*g).max(*b);
    let min = r.min(*g).min(*b);
    let sat = max - min;
    let luma = 0.2126 * *r + 0.7152 * *g + 0.0722 * *b;
    let weight = 1.0 - sat.clamp(0.0, 1.0);
    let boost = 1.0 + amount * weight;
    *r = luma + (*r - luma) * boost;
    *g = luma + (*g - luma) * boost;
    *b = luma + (*b - luma) * boost;
}

/// Apply vibrance to an entire RGBA buffer.
/// amount = 0.0 is identity. Range -1.0 to 1.0.
pub fn apply_vibrance_buffer(rgba: &mut [u8], amount: f32) {
    if amount == 0.0 {
        return;
    }
    let amt = amount as f64;
    for pixel in rgba.chunks_exact_mut(4) {
        let mut r = pixel[0] as f64 / 255.0;
        let mut g = pixel[1] as f64 / 255.0;
        let mut b = pixel[2] as f64 / 255.0;
        apply_vibrance(&mut r, &mut g, &mut b, amt);
        pixel[0] = (r.clamp(0.0, 1.0) * 255.0) as u8;
        pixel[1] = (g.clamp(0.0, 1.0) * 255.0) as u8;
        pixel[2] = (b.clamp(0.0, 1.0) * 255.0) as u8;
    }
}

/// Clarity / midtone detail enhancement.
/// Enhances local contrast in the midtones using an unsharp-mask approach.
/// amount = 0.0 is identity. Range -1.0 to 1.0.
pub fn apply_clarity_inplace(rgba: &mut [u8], width: u32, height: u32, amount: f32) {
    if amount == 0.0 || width == 0 || height == 0 {
        return;
    }
    let w = width as usize;
    let h = height as usize;
    let pixel_count = w * h;

    // 1. Extract luma channel
    let mut luma = vec![0.0f32; pixel_count];
    for (i, chunk) in rgba.chunks_exact(4).enumerate() {
        luma[i] = 0.2126 * chunk[0] as f32 / 255.0
                 + 0.7152 * chunk[1] as f32 / 255.0
                 + 0.0722 * chunk[2] as f32 / 255.0;
    }

    // 2. Box blur (two-pass separable, radius ~20)
    let radius: usize = 20;
    let mut blurred = luma.clone();

    // Horizontal pass
    let mut temp = vec![0.0f32; pixel_count];
    for y in 0..h {
        let row = y * w;
        let mut sum = 0.0f32;
        let mut count = 0u32;
        // Initialize window
        for x in 0..=(radius.min(w - 1)) {
            sum += blurred[row + x];
            count += 1;
        }
        temp[row] = sum / count as f32;
        for x in 1..w {
            // Add right pixel
            let right = x + radius;
            if right < w {
                sum += blurred[row + right];
                count += 1;
            }
            // Remove left pixel
            if x > radius {
                sum -= blurred[row + x - radius - 1];
                count -= 1;
            }
            temp[row + x] = sum / count as f32;
        }
    }
    blurred = temp;

    // Vertical pass
    let mut temp = vec![0.0f32; pixel_count];
    for x in 0..w {
        let mut sum = 0.0f32;
        let mut count = 0u32;
        for y in 0..=(radius.min(h - 1)) {
            sum += blurred[y * w + x];
            count += 1;
        }
        temp[x] = sum / count as f32;
        for y in 1..h {
            let bottom = y + radius;
            if bottom < h {
                sum += blurred[bottom * w + x];
                count += 1;
            }
            if y > radius {
                sum -= blurred[(y - radius - 1) * w + x];
                count -= 1;
            }
            temp[y * w + x] = sum / count as f32;
        }
    }
    blurred = temp;

    // 3. Apply detail enhancement weighted by midtone presence
    for (i, chunk) in rgba.chunks_exact_mut(4).enumerate() {
        let detail = luma[i] - blurred[i];
        // Midtone weight: peaks at 0.5 luma, zero at black/white
        let midtone_weight = 1.0 - (2.0 * luma[i] - 1.0).abs();
        let adjustment = detail * amount * midtone_weight * 255.0;
        chunk[0] = ((chunk[0] as f32 + adjustment).clamp(0.0, 255.0)) as u8;
        chunk[1] = ((chunk[1] as f32 + adjustment).clamp(0.0, 255.0)) as u8;
        chunk[2] = ((chunk[2] as f32 + adjustment).clamp(0.0, 255.0)) as u8;
    }
}

/// Auto white balance on the grey-world assumption: the temperature and
/// tint that make the frame's average colour neutral.
///
/// `rgba` is a displayed frame (sRGB bytes). The average is taken in
/// linear light, and the slider values come from
/// [`crate::white_balance::white_balance_to_neutralise`] — solved
/// against the balance the grade actually applies, rather than a scale
/// factor guessed from the channel differences, which stopped meaning
/// anything when temperature and tint became a chromatic adaptation.
pub fn auto_white_balance(rgba: &[u8], _width: u32, _height: u32) -> (f32, f32) {
    let mut sum = [0.0f64; 3];
    let mut count: u64 = 0;

    // Sample every 4th pixel for performance
    for chunk in rgba.chunks_exact(4).step_by(4) {
        for (acc, v) in sum.iter_mut().zip(&chunk[..3]) {
            *acc += crate::transfer::srgb_to_linear(*v as f32 / 255.0) as f64;
        }
        count += 1;
    }

    if count == 0 {
        return (0.0, 0.0);
    }

    let mean = sum.map(|v| (v / count as f64) as f32);
    crate::white_balance::white_balance_to_neutralise(mean, klipp_core::WorkingColorspace::Rec709Linear)
}

/// Temperature/tint shift.
/// temperature > 0 = warmer (more red, less blue).
/// tint > 0 = more green, < 0 = more magenta.
/// Both 0.0 is identity. Range roughly ±100.
pub fn apply_temperature_tint(rgba: &mut [u8], temperature: f32, tint: f32) {
    if temperature == 0.0 && tint == 0.0 {
        return;
    }
    let t = temperature / 100.0 * 0.1;
    let g = tint / 100.0 * 0.1;
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[0] = ((pixel[0] as f32 / 255.0 + t).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[1] = ((pixel[1] as f32 / 255.0 + g).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[2] = ((pixel[2] as f32 / 255.0 - t).clamp(0.0, 1.0) * 255.0) as u8;
    }
}

/// Tone controls: highlights, shadows, whites, blacks.
/// Each value in -1.0..+1.0, 0.0 is identity.
/// Highlights affect bright pixels (luma > 0.5), shadows affect dark (luma < 0.5).
/// Whites affect near-white (luma > 0.8), blacks affect near-black (luma < 0.2).
pub fn apply_tone_controls(rgba: &mut [u8], highlights: f32, shadows: f32, whites: f32, blacks: f32) {
    if highlights == 0.0 && shadows == 0.0 && whites == 0.0 && blacks == 0.0 {
        return;
    }
    for pixel in rgba.chunks_exact_mut(4) {
        let r = pixel[0] as f32 / 255.0;
        let g = pixel[1] as f32 / 255.0;
        let b = pixel[2] as f32 / 255.0;
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;

        let hi_w = (luma - 0.5).max(0.0) * 2.0;
        let lo_w = (0.5 - luma).max(0.0) * 2.0;
        let white_w = (luma - 0.8).max(0.0) * 5.0;
        let black_w = (0.2 - luma).max(0.0) * 5.0;

        let adj = highlights * hi_w + shadows * lo_w + whites * white_w + blacks * black_w;

        pixel[0] = ((r + adj).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[1] = ((g + adj).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[2] = ((b + adj).clamp(0.0, 1.0) * 255.0) as u8;
    }
}

/// Apply lift/gamma/gain color wheels.
/// Each parameter is a pre-converted (r, g, b) shift from wheel_offset_to_rgb.
/// Lift affects shadows (weight = 1-luma), Gamma midtones (weight = sin(luma*π)),
/// Gain highlights (weight = luma). Uses BT.709 luma.
pub fn apply_lift_gamma_gain(
    rgba: &mut [u8],
    lift: (f32, f32, f32),
    gamma: (f32, f32, f32),
    gain: (f32, f32, f32),
) {
    let is_zero = |v: (f32, f32, f32)| v.0.abs() < 0.0001 && v.1.abs() < 0.0001 && v.2.abs() < 0.0001;
    if is_zero(lift) && is_zero(gamma) && is_zero(gain) {
        return;
    }
    for pixel in rgba.chunks_exact_mut(4) {
        let r = pixel[0] as f32 / 255.0;
        let g = pixel[1] as f32 / 255.0;
        let b = pixel[2] as f32 / 255.0;
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;

        let lift_w = (1.0 - luma).max(0.0);
        let gain_w = luma;
        let gamma_w = (luma * std::f32::consts::PI).sin().max(0.0);

        pixel[0] = ((r + lift.0 * lift_w + gamma.0 * gamma_w + gain.0 * gain_w).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[1] = ((g + lift.1 * lift_w + gamma.1 * gamma_w + gain.1 * gain_w).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[2] = ((b + lift.2 * lift_w + gamma.2 * gamma_w + gain.2 * gain_w).clamp(0.0, 1.0) * 255.0) as u8;
    }
}

/// Apply lift/gamma/gain/offset color wheels.
/// Offset affects all tones uniformly (no luma weighting).
pub fn apply_lift_gamma_gain_offset(
    rgba: &mut [u8],
    lift: (f32, f32, f32),
    gamma: (f32, f32, f32),
    gain: (f32, f32, f32),
    offset: (f32, f32, f32),
) {
    let is_zero = |v: (f32, f32, f32)| v.0.abs() < 0.0001 && v.1.abs() < 0.0001 && v.2.abs() < 0.0001;
    if is_zero(lift) && is_zero(gamma) && is_zero(gain) && is_zero(offset) {
        return;
    }
    for pixel in rgba.chunks_exact_mut(4) {
        let r = pixel[0] as f32 / 255.0;
        let g = pixel[1] as f32 / 255.0;
        let b = pixel[2] as f32 / 255.0;
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;

        let lift_w = (1.0 - luma).max(0.0);
        let gain_w = luma;
        let gamma_w = (luma * std::f32::consts::PI).sin().max(0.0);

        pixel[0] = ((r + lift.0 * lift_w + gamma.0 * gamma_w + gain.0 * gain_w + offset.0).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[1] = ((g + lift.1 * lift_w + gamma.1 * gamma_w + gain.1 * gain_w + offset.1).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[2] = ((b + lift.2 * lift_w + gamma.2 * gamma_w + gain.2 * gain_w + offset.2).clamp(0.0, 1.0) * 255.0) as u8;
    }
}

/// Apply log-domain color wheels (Low/Mid/High).
/// Unlike linear wheels, these operate in log space with smoother, overlapping
/// tonal ranges — better for HDR and log-encoded footage.
/// Low = shadows, Mid = midtones, High = highlights.
pub fn apply_log_wheels(
    rgba: &mut [u8],
    low: (f32, f32, f32),
    mid: (f32, f32, f32),
    high: (f32, f32, f32),
) {
    let is_zero = |v: (f32, f32, f32)| v.0.abs() < 0.0001 && v.1.abs() < 0.0001 && v.2.abs() < 0.0001;
    if is_zero(low) && is_zero(mid) && is_zero(high) {
        return;
    }
    for pixel in rgba.chunks_exact_mut(4) {
        let r = pixel[0] as f32 / 255.0;
        let g = pixel[1] as f32 / 255.0;
        let b = pixel[2] as f32 / 255.0;

        // Convert to log space for tonal range weighting
        // Use a simple log encoding: log_val = log2(linear + 0.001) mapped to ~0-1
        let luma = (0.2126 * r + 0.7152 * g + 0.0722 * b).max(0.001);
        let log_luma = ((luma + 0.001).log2() + 10.0) / 10.0; // maps ~0.001-1.0 to ~0.0-1.0

        // Smooth overlapping tonal weights using Gaussian-like curves in log space
        // Low peaks at log_luma=0.15, Mid at 0.5, High at 0.85
        let low_w = (-(log_luma - 0.15).powi(2) / (2.0 * 0.15_f32.powi(2))).exp();
        let mid_w = (-(log_luma - 0.50).powi(2) / (2.0 * 0.20_f32.powi(2))).exp();
        let high_w = (-(log_luma - 0.85).powi(2) / (2.0 * 0.15_f32.powi(2))).exp();

        pixel[0] = ((r + low.0 * low_w + mid.0 * mid_w + high.0 * high_w).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[1] = ((g + low.1 * low_w + mid.1 * mid_w + high.1 * high_w).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[2] = ((b + low.2 * low_w + mid.2 * mid_w + high.2 * high_w).clamp(0.0, 1.0) * 255.0) as u8;
    }
}

/// Apply per-wheel saturation and brightness adjustments.
/// Each parameter pair (sat, bri) is weighted by the corresponding tonal range.
#[allow(clippy::too_many_arguments)]
pub fn apply_wheel_sat_bri(
    rgba: &mut [u8],
    lift_sat: f32, lift_bri: f32,
    gamma_sat: f32, gamma_bri: f32,
    gain_sat: f32, gain_bri: f32,
    offset_sat: f32, offset_bri: f32,
) {
    if lift_sat.abs() < 0.0001 && lift_bri.abs() < 0.0001
        && gamma_sat.abs() < 0.0001 && gamma_bri.abs() < 0.0001
        && gain_sat.abs() < 0.0001 && gain_bri.abs() < 0.0001
        && offset_sat.abs() < 0.0001 && offset_bri.abs() < 0.0001
    {
        return;
    }
    for pixel in rgba.chunks_exact_mut(4) {
        let r = pixel[0] as f32 / 255.0;
        let g = pixel[1] as f32 / 255.0;
        let b = pixel[2] as f32 / 255.0;
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;

        let lift_w = (1.0 - luma).max(0.0);
        let gamma_w = (luma * std::f32::consts::PI).sin().max(0.0);
        let gain_w = luma;

        let sat_adj = lift_sat * lift_w + gamma_sat * gamma_w + gain_sat * gain_w + offset_sat;
        let bri_adj = lift_bri * lift_w + gamma_bri * gamma_w + gain_bri * gain_w + offset_bri;

        // Apply brightness (uniform shift)
        let r = r + bri_adj;
        let g = g + bri_adj;
        let b = b + bri_adj;

        // Apply saturation adjustment
        let luma2 = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let sat_factor = 1.0 + sat_adj;
        pixel[0] = ((luma2 + (r - luma2) * sat_factor).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[1] = ((luma2 + (g - luma2) * sat_factor).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[2] = ((luma2 + (b - luma2) * sat_factor).clamp(0.0, 1.0) * 255.0) as u8;
    }
}

/// Apply hue rotation in degrees (0-360). Rotates every pixel's hue by the given amount.
pub fn apply_hue_rotation(rgba: &mut [u8], degrees: f32) {
    if degrees.abs() < 0.01 {
        return;
    }
    let angle = degrees / 360.0;
    for pixel in rgba.chunks_exact_mut(4) {
        let r = pixel[0] as f32 / 255.0;
        let g = pixel[1] as f32 / 255.0;
        let b = pixel[2] as f32 / 255.0;

        // RGB → HSL
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;
        let l = (max + min) * 0.5;
        if delta < 0.0001 {
            continue; // achromatic — no hue to rotate
        }
        let s = delta / (1.0 - (2.0 * l - 1.0).abs());
        let h = if max == r {
            ((g - b) / delta).rem_euclid(6.0) / 6.0
        } else if max == g {
            ((b - r) / delta + 2.0) / 6.0
        } else {
            ((r - g) / delta + 4.0) / 6.0
        };

        // Rotate hue
        let h = (h + angle).rem_euclid(1.0);

        // HSL → RGB
        let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
        let x = c * (1.0 - ((h * 6.0) % 2.0 - 1.0).abs());
        let m = l - c * 0.5;
        let (nr, ng, nb) = match (h * 6.0) as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        pixel[0] = ((nr + m).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[1] = ((ng + m).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[2] = ((nb + m).clamp(0.0, 1.0) * 255.0) as u8;
    }
}

/// Apply global mix — blend the graded result with the ungraded original.
pub fn apply_global_mix(graded: &mut [u8], original: &[u8], mix: f32) {
    if (mix - 1.0).abs() < 0.001 || graded.len() != original.len() {
        return;
    }
    for (g, o) in graded.chunks_exact_mut(4).zip(original.chunks_exact(4)) {
        g[0] = ((o[0] as f32 + (g[0] as f32 - o[0] as f32) * mix).clamp(0.0, 255.0)) as u8;
        g[1] = ((o[1] as f32 + (g[1] as f32 - o[1] as f32) * mix).clamp(0.0, 255.0)) as u8;
        g[2] = ((o[2] as f32 + (g[2] as f32 - o[2] as f32) * mix).clamp(0.0, 255.0)) as u8;
    }
}

/// Apply input device transform (IDT) — convert camera log encoding to linear/Rec.709.
/// This is the first step before any grading.
pub fn apply_input_transform(rgba: &mut [u8], log_format: klipp_core::LogFormat) {
    use klipp_core::LogFormat;
    match log_format {
        LogFormat::Srgb | LogFormat::Linear => return,
        _ => {}
    }

    for pixel in rgba.chunks_exact_mut(4) {
        let r = pixel[0] as f32 / 255.0;
        let g = pixel[1] as f32 / 255.0;
        let b = pixel[2] as f32 / 255.0;
        let lr = log_to_linear(r, log_format);
        let lg = log_to_linear(g, log_format);
        let lb = log_to_linear(b, log_format);

        // Linear → sRGB gamma for display/grading in Rec.709
        pixel[0] = (crate::transfer::linear_to_srgb(lr).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[1] = (crate::transfer::linear_to_srgb(lg).clamp(0.0, 1.0) * 255.0) as u8;
        pixel[2] = (crate::transfer::linear_to_srgb(lb).clamp(0.0, 1.0) * 255.0) as u8;
    }
}

/// Camera log / HDR transfer → scene-linear, from a normalised 10-bit code
/// value. Delegates to `camera_log::log_format_to_linear`: this used to be
/// a second copy of every curve, wrong for most of them (S-Log3 read 0.17
/// at mid grey, Canon Log 3 went negative), and disagreeing with the
/// decoder's copy by up to 23 stops.
pub fn log_to_linear(v: f32, format: klipp_core::LogFormat) -> f32 {
    crate::camera_log::log_format_to_linear(v, format)
}

/// Linear → sRGB transfer function.
/// Pre-compute a 256-entry 1D LUT for input transform (log → sRGB).
/// Maps input byte value (0-255) to output sRGB value (0.0-1.0).
/// For non-ACES formats, the transform is per-channel identical so one LUT suffices.
pub fn build_input_transform_lut(log_format: klipp_core::LogFormat) -> [f32; 256] {
    let mut lut = [0.0f32; 256];
    for i in 0..256 {
        let v = i as f32 / 255.0;
        let linear = log_to_linear(v, log_format);
        lut[i] = crate::transfer::linear_to_srgb(linear).clamp(0.0, 1.0);
    }
    lut
}

#[cfg(test)]
mod tests {
    use super::*;
    // Tests in this module reference the sRGB transfer functions by
    // their old unqualified names. They now live in crate::transfer
    // — re-import them so the existing assertions keep compiling.
    use crate::transfer::linear_to_srgb;

    fn make_pixel(r: u8, g: u8, b: u8) -> Vec<u8> {
        vec![r, g, b, 255]
    }

    #[test]
    fn exposure_identity() {
        let mut px = make_pixel(128, 64, 32);
        let orig = px.clone();
        apply_exposure(&mut px, 0.0);
        assert_eq!(px, orig);
    }

    #[test]
    fn exposure_up_brightens() {
        let mut px = make_pixel(100, 100, 100);
        apply_exposure(&mut px, 1.0); // 2x brightness
        assert!(px[0] > 100);
        assert!(px[1] > 100);
        assert!(px[2] > 100);
        assert_eq!(px[3], 255); // alpha unchanged
    }

    #[test]
    fn exposure_down_darkens() {
        let mut px = make_pixel(200, 200, 200);
        apply_exposure(&mut px, -1.0); // 0.5x brightness
        assert!(px[0] < 200);
        assert!(px[1] < 200);
        assert!(px[2] < 200);
    }

    #[test]
    fn exposure_clamps_to_255() {
        let mut px = make_pixel(200, 200, 200);
        apply_exposure(&mut px, 5.0); // huge boost
        assert_eq!(px[0], 255);
        assert_eq!(px[1], 255);
        assert_eq!(px[2], 255);
    }

    #[test]
    fn contrast_identity() {
        let mut px = make_pixel(128, 64, 200);
        let orig = px.clone();
        apply_contrast(&mut px, 0.0);
        assert_eq!(px, orig);
    }

    #[test]
    fn contrast_increases_spread() {
        // Bright pixel gets brighter, dark pixel gets darker
        let mut bright = make_pixel(200, 200, 200);
        let mut dark = make_pixel(50, 50, 50);
        apply_contrast(&mut bright, 0.5);
        apply_contrast(&mut dark, 0.5);
        assert!(bright[0] > 200);
        assert!(dark[0] < 50);
    }

    #[test]
    fn contrast_midpoint_unchanged() {
        // 128 is ~0.502, very close to midpoint 0.5
        let mut px = make_pixel(128, 128, 128);
        apply_contrast(&mut px, 0.5);
        // Should be very close to 128 (midpoint is stable)
        assert!((px[0] as i32 - 128).unsigned_abs() <= 1);
    }

    #[test]
    fn saturation_identity() {
        let mut px = make_pixel(200, 100, 50);
        let orig = px.clone();
        apply_saturation(&mut px, 1.0);
        assert_eq!(px, orig);
    }

    #[test]
    fn saturation_zero_is_grayscale() {
        let mut px = make_pixel(255, 0, 0); // pure red
        apply_saturation(&mut px, 0.0);
        // All channels should be equal (luma)
        assert_eq!(px[0], px[1]);
        assert_eq!(px[1], px[2]);
    }

    #[test]
    fn saturation_boost_increases_color() {
        let mut px = make_pixel(200, 128, 128);
        apply_saturation(&mut px, 2.0);
        // Red channel should increase (it was above luma)
        assert!(px[0] > 200);
        // Green/blue should decrease (they were below luma for this pixel... actually let's check)
        // luma = 0.2126*200/255 + 0.7152*128/255 + 0.0722*128/255 = ~0.525
        // 128/255 = 0.502, so g and b are slightly below luma → they decrease
        assert!(px[1] < 128);
    }

    /// Auto white balance on a frame with a blue cast asks for a warmer
    /// balance, and the values it returns really do neutralise the
    /// average — they are solved against the balance the grade applies,
    /// not scaled from the channel differences.
    #[test]
    fn auto_white_balance_neutralises_a_cast() {
        let cool: Vec<u8> = (0..400).flat_map(|_| [110u8, 120, 150, 255]).collect();
        let (temperature, tint) = auto_white_balance(&cool, 20, 20);
        assert!(temperature > 5.0, "a blue cast should ask for warmth, got {temperature}");
        let mean = [110f32, 120.0, 150.0].map(|v| crate::transfer::srgb_to_linear(v / 255.0));
        let m = crate::white_balance::white_balance_matrix(temperature, tint, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
        let out = m.map(|row| row[0] * mean[0] + row[1] * mean[1] + row[2] * mean[2]);
        let (mx, mn) = (out[0].max(out[1]).max(out[2]), out[0].min(out[1]).min(out[2]));
        assert!((mx - mn) / mx < 0.02, "still a cast: {out:?}");
        // A neutral frame needs no correction.
        let grey: Vec<u8> = (0..400).flat_map(|_| [128u8, 128, 128, 255]).collect();
        let (t, g) = auto_white_balance(&grey, 20, 20);
        assert!(t.abs() < 0.5 && g.abs() < 0.5, "a neutral frame asked for ({t}, {g})");
    }

    #[test]
    fn temperature_identity() {
        let mut px = make_pixel(128, 128, 128);
        let orig = px.clone();
        apply_temperature_tint(&mut px, 0.0, 0.0);
        assert_eq!(px, orig);
    }

    #[test]
    fn temperature_warm_shifts_red() {
        let mut px = make_pixel(128, 128, 128);
        apply_temperature_tint(&mut px, 50.0, 0.0);
        assert!(px[0] > 128); // red increases
        assert_eq!(px[1], 128); // green unchanged
        assert!(px[2] < 128); // blue decreases
    }

    #[test]
    fn temperature_cool_shifts_blue() {
        let mut px = make_pixel(128, 128, 128);
        apply_temperature_tint(&mut px, -50.0, 0.0);
        assert!(px[0] < 128); // red decreases
        assert!(px[2] > 128); // blue increases
    }

    #[test]
    fn tint_positive_shifts_green() {
        let mut px = make_pixel(128, 128, 128);
        apply_temperature_tint(&mut px, 0.0, 50.0);
        assert_eq!(px[0], 128); // red unchanged
        assert!(px[1] > 128); // green increases
        assert_eq!(px[2], 128); // blue unchanged
    }

    #[test]
    fn alpha_never_modified() {
        let mut px = vec![100, 100, 100, 42];
        apply_exposure(&mut px, 1.0);
        assert_eq!(px[3], 42);

        let mut px = vec![100, 100, 100, 42];
        apply_contrast(&mut px, 0.5);
        assert_eq!(px[3], 42);

        let mut px = vec![100, 100, 100, 42];
        apply_saturation(&mut px, 0.5);
        assert_eq!(px[3], 42);

        let mut px = vec![100, 100, 100, 42];
        apply_temperature_tint(&mut px, 50.0, 50.0);
        assert_eq!(px[3], 42);
    }

    #[test]
    fn multi_pixel_buffer() {
        // Ensure it works on a buffer with multiple pixels
        let mut buf = vec![
            100, 100, 100, 255,
            200, 200, 200, 255,
            50, 50, 50, 255,
        ];
        apply_exposure(&mut buf, 1.0);
        assert!(buf[0] > 100); // first pixel brighter
        assert!(buf[4] > 200); // second pixel brighter (or clamped)
        assert!(buf[8] > 50); // third pixel brighter
    }

    // ─── Ramp image helper ───────────────────────────────────────────────

    /// Generate a 256×1 synthetic ramp: R=G=B=i for i in 0..256.
    /// Returns a 256×4 RGBA buffer.
    fn make_ramp() -> Vec<u8> {
        let mut buf = Vec::with_capacity(256 * 4);
        for i in 0..256u16 {
            let v = i as u8;
            buf.extend_from_slice(&[v, v, v, 255]);
        }
        buf
    }

    /// Read the R channel at a given pixel index (R=G=B for grey ramp).
    fn ramp_r(buf: &[u8], idx: usize) -> u8 {
        buf[idx * 4]
    }

    // Pixel indices for 10%, 50%, 90% luma on the ramp (R=G=B=i → luma = i/255).
    const POS_10: usize = 26;  // luma ≈ 0.102
    const POS_50: usize = 128; // luma ≈ 0.502
    const POS_90: usize = 230; // luma ≈ 0.902

    // ─── 1. Lift/Gamma/Gain/Offset ramp tests ───────────────────────────

    #[test]
    fn ramp_lift_only() {
        // Lift at +0.1: output ≈ input + 0.1 × (1 - luma)
        let shift = 0.1f32;
        let mut buf = make_ramp();
        apply_lift_gamma_gain_offset(
            &mut buf,
            (shift, shift, shift),
            (0.0, 0.0, 0.0),
            (0.0, 0.0, 0.0),
            (0.0, 0.0, 0.0),
        );

        // At 10% luma: lift_w = 1 - 0.102 = 0.898, expect ≈ 26 + 0.1*0.898*255 ≈ 48.9
        let luma_10 = POS_10 as f32 / 255.0;
        let expected_10 = (luma_10 + shift * (1.0 - luma_10)) * 255.0;
        let actual_10 = ramp_r(&buf, POS_10) as f32;
        assert!(
            (actual_10 - expected_10).abs() < 2.0,
            "lift at 10%: actual={actual_10}, expected={expected_10}"
        );

        // At 90% luma: lift_w = 1 - 0.902 = 0.098, expect ≈ 230 + 0.1*0.098*255 ≈ 232.5
        let luma_90 = POS_90 as f32 / 255.0;
        let expected_90 = (luma_90 + shift * (1.0 - luma_90)) * 255.0;
        let actual_90 = ramp_r(&buf, POS_90) as f32;
        assert!(
            (actual_90 - expected_90).abs() < 2.0,
            "lift at 90%: actual={actual_90}, expected={expected_90}"
        );

        // Lift should affect shadows MORE than highlights
        let delta_10 = ramp_r(&buf, POS_10) as i16 - POS_10 as i16;
        let delta_90 = ramp_r(&buf, POS_90) as i16 - POS_90 as i16;
        assert!(
            delta_10 > delta_90,
            "lift should affect shadows more: delta_10={delta_10}, delta_90={delta_90}"
        );
    }

    #[test]
    fn ramp_gamma_only() {
        // Gamma at +0.1: maximum effect at 50% luma, zero at 0% and 100%
        let shift = 0.1f32;
        let mut buf = make_ramp();
        apply_lift_gamma_gain_offset(
            &mut buf,
            (0.0, 0.0, 0.0),
            (shift, shift, shift),
            (0.0, 0.0, 0.0),
            (0.0, 0.0, 0.0),
        );

        // At 50% luma: gamma_w = sin(0.5*π) = 1.0, expect ≈ 128 + 0.1*1.0*255 = 153.5
        let luma_50 = POS_50 as f32 / 255.0;
        let gamma_w_50 = (luma_50 * std::f32::consts::PI).sin();
        let expected_50 = (luma_50 + shift * gamma_w_50) * 255.0;
        let actual_50 = ramp_r(&buf, POS_50) as f32;
        assert!(
            (actual_50 - expected_50).abs() < 2.0,
            "gamma at 50%: actual={actual_50}, expected={expected_50}"
        );

        // At 0% luma: gamma_w = sin(0) = 0, pixel 0 should be unchanged
        assert_eq!(
            ramp_r(&buf, 0), 0,
            "gamma should not affect black"
        );

        // At 100% luma: gamma_w = sin(π) ≈ 0, pixel 255 should be unchanged
        assert!(
            (ramp_r(&buf, 255) as i16 - 255).unsigned_abs() <= 1,
            "gamma should not affect white: got {}", ramp_r(&buf, 255)
        );

        // 50% should be most affected (larger delta than 10% and 90%)
        let delta_50 = (ramp_r(&buf, POS_50) as i16 - POS_50 as i16).unsigned_abs();
        let delta_10 = (ramp_r(&buf, POS_10) as i16 - POS_10 as i16).unsigned_abs();
        let delta_90 = (ramp_r(&buf, POS_90) as i16 - POS_90 as i16).unsigned_abs();
        assert!(
            delta_50 >= delta_10 && delta_50 >= delta_90,
            "gamma should peak at midtones: d10={delta_10}, d50={delta_50}, d90={delta_90}"
        );
    }

    #[test]
    fn ramp_gain_only() {
        // Gain at +0.1: output ≈ input + 0.1 × luma
        let shift = 0.1f32;
        let mut buf = make_ramp();
        apply_lift_gamma_gain_offset(
            &mut buf,
            (0.0, 0.0, 0.0),
            (0.0, 0.0, 0.0),
            (shift, shift, shift),
            (0.0, 0.0, 0.0),
        );

        // At 90% luma: gain_w = 0.902, expect ≈ 230 + 0.1*0.902*255 ≈ 253.0
        let luma_90 = POS_90 as f32 / 255.0;
        let expected_90 = (luma_90 + shift * luma_90) * 255.0;
        let actual_90 = ramp_r(&buf, POS_90) as f32;
        assert!(
            (actual_90 - expected_90).abs() < 2.0,
            "gain at 90%: actual={actual_90}, expected={expected_90}"
        );

        // At 10% luma: gain_w = 0.102, expect ≈ 26 + 0.1*0.102*255 ≈ 28.6
        let luma_10 = POS_10 as f32 / 255.0;
        let expected_10 = (luma_10 + shift * luma_10) * 255.0;
        let actual_10 = ramp_r(&buf, POS_10) as f32;
        assert!(
            (actual_10 - expected_10).abs() < 2.0,
            "gain at 10%: actual={actual_10}, expected={expected_10}"
        );

        // Gain should affect highlights MORE than shadows
        let delta_90 = ramp_r(&buf, POS_90) as i16 - POS_90 as i16;
        let delta_10 = ramp_r(&buf, POS_10) as i16 - POS_10 as i16;
        assert!(
            delta_90 > delta_10,
            "gain should affect highlights more: delta_10={delta_10}, delta_90={delta_90}"
        );
    }

    #[test]
    fn ramp_offset_only() {
        // Offset at +0.1: output ≈ input + 0.1 uniformly at all positions
        let shift = 0.1f32;
        let mut buf = make_ramp();
        apply_lift_gamma_gain_offset(
            &mut buf,
            (0.0, 0.0, 0.0),
            (0.0, 0.0, 0.0),
            (0.0, 0.0, 0.0),
            (shift, shift, shift),
        );

        let expected_delta = (shift * 255.0) as i16; // 25

        let delta_10 = ramp_r(&buf, POS_10) as i16 - POS_10 as i16;
        let delta_50 = ramp_r(&buf, POS_50) as i16 - POS_50 as i16;
        let delta_90 = ramp_r(&buf, POS_90) as i16 - POS_90 as i16;

        // All deltas should be approximately equal (uniform shift)
        assert!(
            (delta_10 - expected_delta).unsigned_abs() <= 1,
            "offset at 10%: delta={delta_10}, expected={expected_delta}"
        );
        assert!(
            (delta_50 - expected_delta).unsigned_abs() <= 1,
            "offset at 50%: delta={delta_50}, expected={expected_delta}"
        );
        assert!(
            (delta_90 - expected_delta).unsigned_abs() <= 1,
            "offset at 90%: delta={delta_90}, expected={expected_delta}"
        );

        // Verify uniformity: max spread between any two positions ≤ 1
        let spread = delta_10.max(delta_50).max(delta_90) - delta_10.min(delta_50).min(delta_90);
        assert!(
            spread <= 1,
            "offset should be uniform: deltas=({delta_10}, {delta_50}, {delta_90})"
        );
    }

    // ─── 2. Temperature/tint neutrality ──────────────────────────────────

    #[test]
    fn temperature_tint_neutrality() {
        // Pure grey with temp=0, tint=0 must be unchanged
        let mut px = make_pixel(128, 128, 128);
        apply_temperature_tint(&mut px, 0.0, 0.0);
        assert_eq!(px[0], 128, "R should be unchanged");
        assert_eq!(px[1], 128, "G should be unchanged");
        assert_eq!(px[2], 128, "B should be unchanged");
    }

    #[test]
    fn temperature_warm_symmetric_shift() {
        // With temperature=+50: R increases and B decreases by equal amounts
        let mut px = make_pixel(128, 128, 128);
        apply_temperature_tint(&mut px, 50.0, 0.0);
        let r_delta = px[0] as i16 - 128;
        let b_delta = 128 - px[2] as i16;
        assert!(
            r_delta > 0,
            "warm temperature should increase R: got {}", px[0]
        );
        assert!(
            b_delta > 0,
            "warm temperature should decrease B: got {}", px[2]
        );
        // R increase should equal B decrease (symmetric formula: R += t, B -= t)
        // Allow ±1 for 8-bit truncation asymmetry (floor vs ceil at boundaries)
        assert!(
            (r_delta - b_delta).unsigned_abs() <= 1,
            "R increase ({r_delta}) should approximately equal B decrease ({b_delta})"
        );
        // Green unchanged
        assert_eq!(px[1], 128, "G should be unchanged with tint=0");
    }

    // ─── 3. Saturation identity ──────────────────────────────────────────

    #[test]
    fn saturation_identity_exact() {
        // saturation=1.0 must be a perfect no-op for any input
        let test_pixels: Vec<[u8; 3]> = vec![
            [0, 0, 0],
            [255, 255, 255],
            [128, 128, 128],
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [200, 100, 50],
            [42, 177, 213],
        ];
        for [r, g, b] in &test_pixels {
            let mut px = make_pixel(*r, *g, *b);
            let orig = px.clone();
            apply_saturation(&mut px, 1.0);
            assert_eq!(
                px, orig,
                "saturation=1.0 should be identity for ({r}, {g}, {b})"
            );
        }
    }

    // ─── 4. Curve identity ───────────────────────────────────────────────

    #[test]
    fn curve_identity_perfect_noop() {
        // A curve with only [0,0] and [1,1] must be a perfect no-op
        use crate::curves::{build_curve_lut, apply_curves};

        let identity_lut = build_curve_lut(&[[0.0, 0.0], [1.0, 1.0]]);

        // Verify the LUT itself is identity (every entry maps to itself ±1)
        for i in 0..256 {
            assert!(
                (identity_lut[i] as i16 - i as i16).unsigned_abs() <= 1,
                "identity LUT[{i}] = {}, expected {i}", identity_lut[i]
            );
        }

        // Apply to a variety of pixels — output must equal input exactly
        let test_pixels: Vec<[u8; 3]> = vec![
            [0, 0, 0],
            [255, 255, 255],
            [1, 1, 1],
            [254, 254, 254],
            [128, 64, 200],
            [42, 177, 213],
        ];
        for [r, g, b] in &test_pixels {
            let mut px = make_pixel(*r, *g, *b);
            let orig = px.clone();
            apply_curves(&mut px, &identity_lut, &identity_lut, &identity_lut, &identity_lut);
            // Allow ±1 for the LUT quantization at endpoints
            for ch in 0..3 {
                assert!(
                    (px[ch] as i16 - orig[ch] as i16).unsigned_abs() <= 1,
                    "curve identity failed for ({r},{g},{b}) ch={ch}: got {} expected {}",
                    px[ch], orig[ch]
                );
            }
        }
    }

    // ─── 5. Round-trip (apply + inverse) ─────────────────────────────────

    #[test]
    fn roundtrip_offset() {
        // Apply offset +0.05, then offset -0.05 — should be within 2 LSB
        // (two 8-bit quantization steps: forward truncation + inverse truncation)
        let mut buf = make_ramp();
        let original = buf.clone();

        apply_lift_gamma_gain_offset(
            &mut buf,
            (0.0, 0.0, 0.0),
            (0.0, 0.0, 0.0),
            (0.0, 0.0, 0.0),
            (0.05, 0.05, 0.05),
        );
        apply_lift_gamma_gain_offset(
            &mut buf,
            (0.0, 0.0, 0.0),
            (0.0, 0.0, 0.0),
            (0.0, 0.0, 0.0),
            (-0.05, -0.05, -0.05),
        );

        // Check every non-clamped pixel (skip near 255 where clamping occurs)
        for i in 0..243 {
            let orig_val = original[i * 4] as i16;
            let rt_val = buf[i * 4] as i16;
            let err = (rt_val - orig_val).unsigned_abs();
            assert!(
                err <= 2,
                "round-trip offset pixel {i}: original={orig_val}, got={rt_val}, err={err}"
            );
        }
    }

    #[test]
    fn roundtrip_temperature() {
        // Apply temperature +20, then -20
        let mut px = make_pixel(128, 128, 128);
        let orig = px.clone();
        apply_temperature_tint(&mut px, 20.0, 0.0);
        apply_temperature_tint(&mut px, -20.0, 0.0);
        for ch in 0..3 {
            let err = (px[ch] as i16 - orig[ch] as i16).unsigned_abs();
            assert!(
                err <= 2,
                "round-trip temperature ch={ch}: original={}, got={}, err={err}",
                orig[ch], px[ch]
            );
        }
    }

    #[test]
    fn roundtrip_lift_gain() {
        // Apply lift +0.05 and gain +0.05, then negate both
        let mut buf = make_ramp();
        let original = buf.clone();

        let shift = 0.05f32;
        apply_lift_gamma_gain_offset(
            &mut buf,
            (shift, shift, shift),
            (0.0, 0.0, 0.0),
            (shift, shift, shift),
            (0.0, 0.0, 0.0),
        );
        apply_lift_gamma_gain_offset(
            &mut buf,
            (-shift, -shift, -shift),
            (0.0, 0.0, 0.0),
            (-shift, -shift, -shift),
            (0.0, 0.0, 0.0),
        );

        // Check non-clamped range (skip extremes where clamping loses info)
        for i in 5..240 {
            let orig_val = original[i * 4] as i16;
            let rt_val = buf[i * 4] as i16;
            let err = (rt_val - orig_val).unsigned_abs();
            assert!(
                err <= 2,
                "round-trip lift+gain pixel {i}: original={orig_val}, got={rt_val}, err={err}"
            );
        }
    }

    #[test]
    fn roundtrip_exposure() {
        // Apply exposure +0.25 stops, then -0.25 stops
        let mut buf = make_ramp();
        let original = buf.clone();

        apply_exposure(&mut buf, 0.25);
        // Snapshot the forward pass to detect clipped pixels
        let after_forward = buf.clone();
        apply_exposure(&mut buf, -0.25);

        // Multiplicative round-trip: 2^0.25 * 2^-0.25 = 1, but 8-bit truncation
        // Skip pixels that clipped to 0 or 255 in the forward pass (information lost)
        for i in 1..256 {
            let fwd = after_forward[i * 4];
            if fwd == 0 || fwd == 255 {
                continue; // clipped — can't recover
            }
            let orig_val = original[i * 4] as i16;
            let rt_val = buf[i * 4] as i16;
            let err = (rt_val - orig_val).unsigned_abs();
            assert!(
                err <= 2,
                "round-trip exposure pixel {i}: original={orig_val}, got={rt_val}, err={err}"
            );
        }
    }

    #[test]
    fn roundtrip_contrast() {
        // Apply contrast +0.2, then -0.2 (not exact inverse but close for small values)
        // Inverse of scale=(1+0.2) is scale=1/(1.2)=0.833, so factor = 0.833-1 = -0.167
        let mut px = make_pixel(100, 150, 200);
        let orig = px.clone();
        let factor = 0.2f32;
        let inv_factor = 1.0 / (1.0 + factor) - 1.0;
        apply_contrast(&mut px, factor);
        apply_contrast(&mut px, inv_factor);
        for ch in 0..3 {
            let err = (px[ch] as i16 - orig[ch] as i16).unsigned_abs();
            assert!(
                err <= 2,
                "round-trip contrast ch={ch}: original={}, got={}, err={err}",
                orig[ch], px[ch]
            );
        }
    }

    // ─── HDR/ACES input transform tests ──────────────────────────────

    #[test]
    fn slog3_black_point() {
        // S-Log3 encoded black (value=0) should map to near-zero linear
        let mut px = make_pixel(0, 0, 0);
        apply_input_transform(&mut px, klipp_core::LogFormat::SLog3);
        // Black should stay dark (output ≤ 10 after sRGB gamma)
        assert!(px[0] <= 10, "S-Log3 black should stay dark: got {}", px[0]);
    }

    #[test]
    fn slog3_middle_grey() {
        // S-Log3: 0.41 encoded ≈ 18% middle grey
        // 0.41 * 255 ≈ 104.55 → pixel value 105
        let mut px = make_pixel(105, 105, 105);
        apply_input_transform(&mut px, klipp_core::LogFormat::SLog3);
        // After log→linear→sRGB: 18% linear ≈ 0.18, sRGB(0.18) ≈ 0.461 → ~118
        // Tolerance: ±15 because the S-Log3 formula is an approximation
        // and the 8-bit input quantisation at 105 doesn't land exactly at 0.41
        let output = px[0] as f32 / 255.0;
        assert!(output > 0.2 && output < 0.7,
            "S-Log3 middle grey should be mid-range: got {} ({})", px[0], output);
    }

    #[test]
    fn slog3_white_point() {
        // S-Log3: ~0.70 encoded ≈ 90% white. 0.70*255 ≈ 178
        let mut px = make_pixel(178, 178, 178);
        apply_input_transform(&mut px, klipp_core::LogFormat::SLog3);
        // Should be brighter than the input value (log expands highlights)
        assert!(px[0] > 178, "S-Log3 white should expand: got {}", px[0]);
    }

    #[test]
    fn logc3_middle_grey() {
        // ARRI LogC3 at EI800: middle grey (0.18 linear) encodes to ~0.391
        // 0.391 * 255 ≈ 100
        let mut px = make_pixel(100, 100, 100);
        apply_input_transform(&mut px, klipp_core::LogFormat::LogC3);
        let output = px[0] as f32 / 255.0;
        // Should land in the mid-range after conversion
        assert!(output > 0.2 && output < 0.7,
            "LogC3 middle grey should be mid-range: got {} ({})", px[0], output);
    }

    #[test]
    fn log_transform_monotone() {
        // Log transforms applied to ascending values should produce monotonically
        // increasing output. Test the most common formats.
        for format in [klipp_core::LogFormat::SLog3, klipp_core::LogFormat::LogC3,
                       klipp_core::LogFormat::VLog, klipp_core::LogFormat::SLog2] {
            let mut prev = 0u8;
            for i in 0..=255u8 {
                let mut px = vec![i, i, i, 255];
                apply_input_transform(&mut px, format);
                assert!(px[0] >= prev,
                    "{:?}: monotonicity violation at input {i}: output {} < prev {prev}",
                    format, px[0]);
                prev = px[0];
            }
        }
    }

    #[test]
    fn log_transform_no_negative_output() {
        // All log transforms should produce non-negative output for any input
        for format in [klipp_core::LogFormat::SLog3, klipp_core::LogFormat::SLog2,
                       klipp_core::LogFormat::LogC3, klipp_core::LogFormat::LogC4,
                       klipp_core::LogFormat::VLog, klipp_core::LogFormat::CanonLog,
                       klipp_core::LogFormat::CanonLog3, klipp_core::LogFormat::NLog,
                       klipp_core::LogFormat::FLog, klipp_core::LogFormat::DJIDLog] {
            for i in 0..=255u8 {
                let mut px = vec![i, i, i, 255];
                apply_input_transform(&mut px, format);
                // Output is u8 so it's inherently non-negative, but verify no panic
                // and alpha is preserved
                assert_eq!(px[3], 255, "{:?}: alpha corrupted at input {i}", format);
            }
        }
    }

    // ─── Film grain CPU tests ─────────────────────────────────────────

    fn wang_hash_cpu(mut x: u32) -> f32 {
        x = (x ^ 61) ^ (x >> 16);
        x = x.wrapping_mul(9);
        x = x ^ (x >> 4);
        x = x.wrapping_mul(0x27d4eb2d);
        x = x ^ (x >> 15);
        x as f32 / u32::MAX as f32
    }

    fn apply_grain_cpu(rgba: &mut [u8], width: u32, amount: f32, size: f32,
                       shadows: f32, highlights: f32, monochrome: bool, frame_seed: u32) {
        for (i, chunk) in rgba.chunks_exact_mut(4).enumerate() {
            let px = (i as u32) % width;
            let py = (i as u32) / width;
            let qx = (px as f32 / size) as u32;
            let qy = (py as f32 / size) as u32;
            let seed = qx.wrapping_mul(1973).wrapping_add(qy.wrapping_mul(9277))
                .wrapping_add(frame_seed.wrapping_mul(26699));
            let noise_r = (wang_hash_cpu(seed) - 0.5) * amount;
            let noise_g = if monochrome { noise_r } else { (wang_hash_cpu(seed + 1) - 0.5) * amount };
            let noise_b = if monochrome { noise_r } else { (wang_hash_cpu(seed + 2) - 0.5) * amount };
            let luma = 0.2126 * chunk[0] as f32 / 255.0 + 0.7152 * chunk[1] as f32 / 255.0
                + 0.0722 * chunk[2] as f32 / 255.0;
            let weight = ((1.0 - luma) * shadows + luma * highlights).min(1.5);
            chunk[0] = ((chunk[0] as f32 / 255.0 + noise_r * weight).clamp(0.0, 1.0) * 255.0) as u8;
            chunk[1] = ((chunk[1] as f32 / 255.0 + noise_g * weight).clamp(0.0, 1.0) * 255.0) as u8;
            chunk[2] = ((chunk[2] as f32 / 255.0 + noise_b * weight).clamp(0.0, 1.0) * 255.0) as u8;
        }
    }

    #[test]
    fn grain_zero_amount_is_noop() {
        let mut px = vec![100, 150, 200, 255, 50, 80, 120, 255];
        let orig = px.clone();
        apply_grain_cpu(&mut px, 2, 0.0, 1.0, 1.0, 0.5, true, 42);
        assert_eq!(px, orig, "zero grain should be exact no-op");
    }

    #[test]
    fn grain_deterministic_per_seed() {
        let mut px1 = vec![128; 400]; // 100 pixels
        let mut px2 = px1.clone();
        apply_grain_cpu(&mut px1, 10, 0.2, 1.0, 1.0, 0.5, true, 42);
        apply_grain_cpu(&mut px2, 10, 0.2, 1.0, 1.0, 0.5, true, 42);
        assert_eq!(px1, px2, "same seed should produce identical results");
    }

    #[test]
    fn grain_varies_between_frames() {
        let mut px1 = vec![128; 400];
        let mut px2 = vec![128; 400];
        apply_grain_cpu(&mut px1, 10, 0.2, 1.0, 1.0, 0.5, true, 0);
        apply_grain_cpu(&mut px2, 10, 0.2, 1.0, 1.0, 0.5, true, 1);
        assert_ne!(px1, px2, "different seeds should produce different results");
    }

    #[test]
    fn grain_monochrome_equal_channels() {
        let mut px = vec![128, 128, 128, 255];
        let orig = px.clone();
        apply_grain_cpu(&mut px, 1, 0.3, 1.0, 1.0, 1.0, true, 7);
        // Monochrome: all channels should shift by the same amount
        let dr = px[0] as i16 - orig[0] as i16;
        let dg = px[1] as i16 - orig[1] as i16;
        let db = px[2] as i16 - orig[2] as i16;
        assert_eq!(dr, dg, "mono grain R={dr} should equal G={dg}");
        assert_eq!(dg, db, "mono grain G={dg} should equal B={db}");
    }

    #[test]
    fn grain_colour_independent_channels() {
        // Colour grain: R, G, B should get independent noise
        let mut px = vec![128, 128, 128, 255];
        apply_grain_cpu(&mut px, 1, 0.3, 1.0, 1.0, 1.0, false, 7);
        // With independent hashes, at least one channel pair should differ
        // (statistically near-impossible for all 3 to be identical)
        let dr = px[0] as i16 - 128;
        let dg = px[1] as i16 - 128;
        let db = px[2] as i16 - 128;
        let all_same = dr == dg && dg == db;
        assert!(!all_same, "colour grain channels should be independent: dr={dr} dg={dg} db={db}");
    }

    #[test]
    fn grain_clamped() {
        // Max grain on near-black and near-white pixels — must not overflow
        let mut px = vec![2, 2, 2, 255, 253, 253, 253, 255];
        apply_grain_cpu(&mut px, 2, 1.0, 1.0, 1.0, 1.0, true, 99);
        // All values must be valid u8 (0..=255) — the type guarantees this,
        // but verify no panic occurred and alpha is preserved
        assert_eq!(px[3], 255);
        assert_eq!(px[7], 255);
    }

    #[test]
    fn grain_shadow_weight() {
        // shadows=1.0, highlights=0.0: dark pixels should show more grain
        let mut dark = vec![10, 10, 10, 255];
        let mut bright = vec![245, 245, 245, 255];
        apply_grain_cpu(&mut dark, 1, 0.3, 1.0, 1.0, 0.0, true, 42);
        apply_grain_cpu(&mut bright, 1, 0.3, 1.0, 1.0, 0.0, true, 42);
        let dark_change = (dark[0] as i16 - 10).unsigned_abs();
        let bright_change = (bright[0] as i16 - 245).unsigned_abs();
        // With highlights=0, bright pixels get zero weight → no grain
        // Dark pixels get full shadow weight → grain applied
        assert!(dark_change >= bright_change,
            "dark pixel change ({dark_change}) should >= bright ({bright_change}) with highlights=0");
    }

    // ─── Input transform / log-to-linear tests ─────────────────────────

    /// Helper: apply_input_transform to a single pixel and return (r, g, b) as f32 in 0..1.
    fn transform_pixel(r: f32, g: f32, b: f32, format: klipp_core::LogFormat) -> (f32, f32, f32) {
        let mut px = vec![
            (r.clamp(0.0, 1.0) * 255.0) as u8,
            (g.clamp(0.0, 1.0) * 255.0) as u8,
            (b.clamp(0.0, 1.0) * 255.0) as u8,
            255,
        ];
        apply_input_transform(&mut px, format);
        (px[0] as f32 / 255.0, px[1] as f32 / 255.0, px[2] as f32 / 255.0)
    }

    #[test]
    fn input_transform_srgb_passthrough() {
        // Srgb format should return input unchanged
        let mut px = vec![100, 150, 200, 255];
        let orig = px.clone();
        apply_input_transform(&mut px, klipp_core::LogFormat::Srgb);
        assert_eq!(px, orig, "Srgb should be a no-op passthrough");
    }

    #[test]
    fn input_transform_linear_passthrough() {
        // Linear format should return input unchanged
        let mut px = vec![100, 150, 200, 255];
        let orig = px.clone();
        apply_input_transform(&mut px, klipp_core::LogFormat::Linear);
        assert_eq!(px, orig, "Linear should be a no-op passthrough");
    }

    #[test]
    fn log_to_linear_slog3_18_percent_gray() {
        // S-Log3 encodes 18% gray (0.18 linear) at approximately 0.4 code value.
        // The log_to_linear function should map ~0.4 → ~0.18 linear.
        let linear = log_to_linear(0.4, klipp_core::LogFormat::SLog3);
        assert!(
            (linear - 0.18).abs() < 0.03,
            "SLog3(0.4) should be near 0.18 (18% gray), got {linear}"
        );
        // Must be positive and finite
        assert!(linear > 0.0 && linear.is_finite());
    }

    #[test]
    fn log_to_linear_logc3_18_percent_gray() {
        // LogC3 (EI 800): 18% gray at ~0.391 code value
        let linear = log_to_linear(0.391, klipp_core::LogFormat::LogC3);
        assert!(
            (linear - 0.18).abs() < 0.05,
            "LogC3(0.391) should be near 0.18, got {linear}"
        );
        assert!(linear > 0.0 && linear.is_finite());
    }

    #[test]
    fn log_to_linear_full_range_no_nan_inf() {
        // For all log formats, 0.0 and 1.0 should produce finite values.
        use klipp_core::LogFormat;
        let formats = [
            LogFormat::SLog3, LogFormat::SLog2, LogFormat::LogC3,
            LogFormat::LogC4, LogFormat::CanonLog, LogFormat::CanonLog3,
            LogFormat::VLog, LogFormat::NLog, LogFormat::FLog,
            LogFormat::DJIDLog, LogFormat::AppleLog,
        ];
        for fmt in &formats {
            let at_zero = log_to_linear(0.0, *fmt);
            let at_one = log_to_linear(1.0, *fmt);
            assert!(
                at_zero.is_finite(),
                "log_to_linear(0.0, {fmt:?}) produced non-finite: {at_zero}"
            );
            assert!(
                at_one.is_finite(),
                "log_to_linear(1.0, {fmt:?}) produced non-finite: {at_one}"
            );
            // 1.0 in log space should map to a positive linear value
            assert!(
                at_one > 0.0,
                "log_to_linear(1.0, {fmt:?}) should be positive, got {at_one}"
            );
        }
    }

    #[test]
    fn log_to_linear_zero_maps_near_zero() {
        // For most log formats, 0.0 code value should map to near-zero or small negative
        // (some formats like CanonLog produce larger negative values below black).
        use klipp_core::LogFormat;
        let formats_tight = [
            LogFormat::SLog3, LogFormat::LogC3, LogFormat::VLog,
            LogFormat::CanonLog3,
        ];
        for fmt in &formats_tight {
            let val = log_to_linear(0.0, *fmt);
            assert!(
                val.abs() < 0.1,
                "log_to_linear(0.0, {fmt:?}) should be near zero, got {val}"
            );
        }
        // CanonLog produces a larger negative at 0.0 (below-black region) — just verify finite
        let canon_val = log_to_linear(0.0, LogFormat::CanonLog);
        assert!(canon_val.is_finite(), "CanonLog(0.0) should be finite, got {canon_val}");
        assert!(canon_val < 0.0, "CanonLog(0.0) should be negative (below black)");
    }

    #[test]
    fn round_trip_linear_to_srgb_and_back() {
        // Both directions of the transfer curve now live in
        // crate::transfer and have their own thorough tests. The
        // spot-check below stays to catch any accidental rewiring
        // that pulls the wrong function into transforms.rs.
        use crate::transfer::srgb_to_linear;
        assert!((linear_to_srgb(0.0) - 0.0).abs() < 0.001);
        assert!((linear_to_srgb(1.0) - 1.0).abs() < 0.001);
        let srgb_18 = linear_to_srgb(0.18);
        assert!(
            (srgb_18 - 0.4613).abs() < 0.01,
            "linear_to_srgb(0.18) should be ~0.4613, got {srgb_18}"
        );
        // Forward direction is now exposed too.
        assert!((srgb_to_linear(0.4613) - 0.18).abs() < 0.005);
    }

    #[test]
    fn round_trip_srgb_via_input_transform() {
        // If we encode a value with linear_to_srgb and then decode through
        // apply_input_transform with a log format that is identity + linear_to_srgb,
        // we should get back close to the original.
        // SLog3 at its 18% gray point: log_to_linear(0.4) ≈ 0.18,
        // then linear_to_srgb(0.18) ≈ 0.4613 → pixel ≈ 118.
        // This is a one-way sanity check that the pipeline produces reasonable values.
        let (r, _, _) = transform_pixel(0.4, 0.4, 0.4, klipp_core::LogFormat::SLog3);
        // SLog3 0.4 → ~0.18 linear → ~0.46 sRGB
        assert!(
            (r - 0.46).abs() < 0.05,
            "SLog3 pipeline: 0.4 input should yield ~0.46 sRGB, got {r}"
        );
    }

    #[test]
    fn input_transform_preserves_alpha() {
        use klipp_core::LogFormat;
        let formats = [
            LogFormat::SLog3, LogFormat::LogC3, LogFormat::SLog2,
            LogFormat::LogC4, LogFormat::VLog,
        ];
        for fmt in &formats {
            let mut px = vec![128, 128, 128, 42];
            apply_input_transform(&mut px, *fmt);
            assert_eq!(px[3], 42, "Alpha should be preserved for {fmt:?}");
        }
    }

    // ─── Exposure precise value tests ───────────────────────────────────

    #[test]
    fn exposure_plus_one_stop_approximately_doubles() {
        // +1 stop should multiply by 2.0
        // Input 100/255 ≈ 0.392, ×2 = 0.784 → ~200
        let mut px = make_pixel(100, 100, 100);
        apply_exposure(&mut px, 1.0);
        let expected = ((100.0 / 255.0 * 2.0) * 255.0) as u8; // 200
        assert_eq!(px[0], expected, "+1 stop: 100 should become {expected}");
    }

    #[test]
    fn exposure_minus_one_stop_approximately_halves() {
        // -1 stop should multiply by 0.5
        let mut px = make_pixel(200, 200, 200);
        apply_exposure(&mut px, -1.0);
        let expected = ((200.0 / 255.0 * 0.5) * 255.0) as u8; // 100
        assert_eq!(px[0], expected, "-1 stop: 200 should become {expected}");
    }

    // ─── Contrast precise behavior ──────────────────────────────────────

    #[test]
    fn contrast_zero_is_identity() {
        let mut px = make_pixel(73, 191, 42);
        let orig = px.clone();
        apply_contrast(&mut px, 0.0);
        assert_eq!(px, orig);
    }

    #[test]
    fn contrast_positive_expands_range() {
        // With positive contrast, values above midpoint move up, below move down
        let mut px_bright = make_pixel(200, 200, 200);
        let mut px_dark = make_pixel(50, 50, 50);
        apply_contrast(&mut px_bright, 1.0);
        apply_contrast(&mut px_dark, 1.0);
        // 200/255 = 0.784, deviation from 0.5 = 0.284, ×2.0 + 0.5 = 1.069 → clamped 255
        assert!(px_bright[0] > 200);
        // 50/255 = 0.196, deviation from 0.5 = -0.304, ×2.0 + 0.5 = -0.108 → clamped 0
        assert!(px_dark[0] < 50);
    }

    // ─── Saturation precise behavior ────────────────────────────────────

    #[test]
    fn saturation_one_is_identity() {
        let mut px = make_pixel(200, 100, 50);
        let orig = px.clone();
        apply_saturation(&mut px, 1.0);
        assert_eq!(px, orig);
    }

    #[test]
    fn saturation_zero_produces_grayscale() {
        // Pure red should become a uniform gray at luma value
        let mut px = make_pixel(255, 0, 0);
        apply_saturation(&mut px, 0.0);
        // BT.709 luma: 0.2126*1.0 + 0.7152*0.0 + 0.0722*0.0 = 0.2126 → ~54
        let expected_luma = (0.2126 * 255.0) as u8;
        assert_eq!(px[0], expected_luma);
        assert_eq!(px[1], expected_luma);
        assert_eq!(px[2], expected_luma);
    }

    #[test]
    fn saturation_zero_neutral_unchanged() {
        // Already grayscale input should be unchanged at any saturation
        let mut px = make_pixel(128, 128, 128);
        apply_saturation(&mut px, 0.0);
        // All channels equal to luma, which equals 128 for neutral
        assert_eq!(px[0], 128);
        assert_eq!(px[1], 128);
        assert_eq!(px[2], 128);
    }

    // ─── Log format monotonicity ────────────────────────────────────────

    #[test]
    fn log_to_linear_monotonic() {
        // For most formats, log_to_linear should be monotonically increasing
        // (higher code value → higher linear value).
        // Note: CanonLog3 has a known piecewise discontinuity at its cutpoint
        // (~0.097) where the linear segment meets the log segment, so we
        // exclude it from strict monotonicity testing.
        use klipp_core::LogFormat;
        let formats = [
            LogFormat::SLog3, LogFormat::SLog2, LogFormat::LogC3,
            LogFormat::LogC4, LogFormat::CanonLog,
            LogFormat::VLog, LogFormat::FLog,
            LogFormat::AppleLog,
        ];
        for fmt in &formats {
            let mut prev = log_to_linear(0.0, *fmt);
            for i in 1..=100 {
                let v = i as f32 / 100.0;
                let cur = log_to_linear(v, *fmt);
                assert!(
                    cur >= prev,
                    "log_to_linear not monotonic for {fmt:?}: f({}) = {cur} < f({}) = {prev}",
                    v, (i - 1) as f32 / 100.0
                );
                prev = cur;
            }
        }
    }

    #[test]
    fn log_to_linear_canon_log3_above_cutpoint_monotonic() {
        // CanonLog3 has a piecewise discontinuity at ~0.097, but above the cut region
        // (code value > 0.1) it should be monotonic.
        use klipp_core::LogFormat;
        let mut prev = log_to_linear(0.1, LogFormat::CanonLog3);
        for i in 11..=100 {
            let v = i as f32 / 100.0;
            let cur = log_to_linear(v, LogFormat::CanonLog3);
            assert!(
                cur >= prev,
                "CanonLog3 not monotonic above cut: f({v}) = {cur} < f({}) = {prev}",
                (i - 1) as f32 / 100.0
            );
            prev = cur;
        }
    }

    #[test]
    fn log_to_linear_dji_dlog_above_cutpoint_monotonic() {
        // DJI D-Log has a piecewise discontinuity at 0.14, but above the cut region
        // it should be monotonic.
        use klipp_core::LogFormat;
        let mut prev = log_to_linear(0.15, LogFormat::DJIDLog);
        for i in 16..=100 {
            let v = i as f32 / 100.0;
            let cur = log_to_linear(v, LogFormat::DJIDLog);
            assert!(
                cur >= prev,
                "DJIDLog not monotonic above cut: f({v}) = {cur} < f({}) = {prev}",
                (i - 1) as f32 / 100.0
            );
            prev = cur;
        }
    }

    // ─── linear_to_srgb properties ──────────────────────────────────────

    #[test]
    fn linear_to_srgb_boundary_values() {
        // Known sRGB transfer function properties
        assert!((linear_to_srgb(0.0)).abs() < 1e-6, "sRGB(0) = 0");
        assert!((linear_to_srgb(1.0) - 1.0).abs() < 1e-4, "sRGB(1) = 1");
        // The linear segment ends at 0.0031308
        let at_cut = linear_to_srgb(0.003_130_8);
        assert!(at_cut > 0.0 && at_cut < 0.05, "sRGB at cutoff should be small positive");
    }

    #[test]
    fn linear_to_srgb_monotonic() {
        let mut prev = linear_to_srgb(0.0);
        for i in 1..=1000 {
            let v = i as f32 / 1000.0;
            let cur = linear_to_srgb(v);
            assert!(cur >= prev, "linear_to_srgb not monotonic at {v}");
            prev = cur;
        }
    }

    // ─── Multi-pixel input transform ────────────────────────────────────

    #[test]
    fn input_transform_multi_pixel() {
        // Ensure the transform processes all pixels in a buffer, not just the first
        let mut buf = vec![
            102, 102, 102, 255, // pixel 0
            153, 153, 153, 255, // pixel 1
            204, 204, 204, 255, // pixel 2
        ];
        let orig = buf.clone();
        apply_input_transform(&mut buf, klipp_core::LogFormat::SLog3);
        // All pixels should have changed (SLog3 is not identity)
        assert_ne!(&buf[0..3], &orig[0..3], "pixel 0 should change");
        assert_ne!(&buf[4..7], &orig[4..7], "pixel 1 should change");
        assert_ne!(&buf[8..11], &orig[8..11], "pixel 2 should change");
        // All alphas preserved
        assert_eq!(buf[3], 255);
        assert_eq!(buf[7], 255);
        assert_eq!(buf[11], 255);
    }
}
