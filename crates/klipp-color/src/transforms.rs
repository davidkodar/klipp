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

#[cfg(test)]
mod tests {
    use super::*;

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
}
