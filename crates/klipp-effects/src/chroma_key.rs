use crate::SpillMethod;

/// Chroma key + spill suppression. Two-stage transform:
///   1. **Matte stage** — distance from the user-picked `key_color`
///      in BT.601 CbCr space drives the alpha. Pixels closer than
///      `tolerance` are fully keyed out; pixels in the soft band
///      `[tolerance, tolerance + soft_edge]` get a linear alpha
///      ramp; pixels past that are unchanged.
///   2. **Spill stage** — pixels still visible (alpha > 0) get the
///      key-colour cast removed from their RGB via `spill_method`
///      / `spill_strength`. Skipped entirely when method is `None`
///      or strength is zero (back-compat default).
///
/// `soft_edge` was previously called `spill_suppression`; the rename
/// matches what the parameter actually does (it has only ever
/// modulated alpha — real spill suppression is a separate stage now).
pub fn apply(
    mut rgba: Vec<u8>,
    key_color: [u8; 3],
    tolerance: f32,
    soft_edge: f32,
    spill_method: SpillMethod,
    spill_strength: f32,
) -> Vec<u8> {
    let (key_cb, key_cr) = rgb_to_cbcr(key_color[0], key_color[1], key_color[2]);
    let key_idx = dominant_channel(key_color);
    let spill_active = !matches!(spill_method, SpillMethod::None) && spill_strength > 0.0;

    for pixel in rgba.chunks_exact_mut(4) {
        let (cb, cr) = rgb_to_cbcr(pixel[0], pixel[1], pixel[2]);

        // Distance in CbCr space (normalized 0–1)
        let dcb = (cb - key_cb) / 255.0;
        let dcr = (cr - key_cr) / 255.0;
        let dist = (dcb * dcb + dcr * dcr).sqrt();

        if dist < tolerance {
            // Fully keyed out — no spill work needed.
            pixel[0] = 0;
            pixel[1] = 0;
            pixel[2] = 0;
            pixel[3] = 0;
            continue;
        } else if dist < tolerance + soft_edge {
            // Soft alpha edge.
            let factor = (dist - tolerance) / soft_edge;
            pixel[3] = (pixel[3] as f32 * factor).clamp(0.0, 255.0) as u8;
        }
        // Beyond tolerance + soft_edge → alpha untouched.

        // Spill suppression on whatever's still visible.
        if spill_active && pixel[3] > 0 {
            apply_spill(pixel, key_idx, spill_method, spill_strength);
        }
    }
    rgba
}

/// Suppress spill on `pixel` for the given key channel + method.
/// Modifies the key channel in place; other channels untouched. The
/// method picks a target value the key channel drops toward when it
/// exceeds that target. `strength` blends key → target linearly.
#[inline]
fn apply_spill(pixel: &mut [u8], key_idx: usize, method: SpillMethod, strength: f32) {
    let r = pixel[0] as f32;
    let g = pixel[1] as f32;
    let b = pixel[2] as f32;
    let channels = [r, g, b];
    let key = channels[key_idx];
    let other_a = channels[(key_idx + 1) % 3];
    let other_b = channels[(key_idx + 2) % 3];

    let target = match method {
        SpillMethod::None => return,
        SpillMethod::ChannelClamp => other_a.max(other_b),
        SpillMethod::Hawk => 0.5 * (other_a + other_b),
    };

    if key > target {
        let new_key = key + strength * (target - key); // strength × (negative) → drops
        pixel[key_idx] = new_key.clamp(0.0, 255.0) as u8;
    }
}

/// Identify the dominant channel of an RGB triplet — used to pick
/// which channel the spill methods clamp. For pure green key
/// `[0, 255, 0]` returns 1 (G); for pure blue `[0, 0, 255]` returns 2.
/// Ties resolve to the first max in R/G/B order, deterministic.
#[inline]
fn dominant_channel(rgb: [u8; 3]) -> usize {
    let (r, g, b) = (rgb[0], rgb[1], rgb[2]);
    if g >= r && g >= b {
        1
    } else if b > r {
        2
    } else {
        0
    }
}

/// Convert RGB to CbCr (BT.601). Returns (Cb, Cr) in 0–255 range.
fn rgb_to_cbcr(r: u8, g: u8, b: u8) -> (f32, f32) {
    let rf = r as f32;
    let gf = g as f32;
    let bf = b as f32;
    let cb = 128.0 + (-0.168736 * rf - 0.331264 * gf + 0.5 * bf);
    let cr = 128.0 + (0.5 * rf - 0.418688 * gf - 0.081312 * bf);
    (cb, cr)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Convenience: alpha-only legacy shape (no spill suppression),
    /// matching the original `apply` signature so the original
    /// behavioural tests stay intact under the new API.
    fn apply_no_spill(rgba: Vec<u8>, key: [u8; 3], tol: f32, soft: f32) -> Vec<u8> {
        apply(rgba, key, tol, soft, SpillMethod::None, 0.0)
    }

    #[test]
    fn pure_green_keyed_out() {
        let input = vec![0, 255, 0, 255]; // pure green pixel
        let output = apply_no_spill(input, [0, 255, 0], 0.3, 0.2);
        assert_eq!(output[3], 0); // should be fully transparent
    }

    #[test]
    fn non_key_color_preserved() {
        let input = vec![255, 0, 0, 255]; // pure red pixel
        let output = apply_no_spill(input.clone(), [0, 255, 0], 0.3, 0.2);
        // Red is far from green in CbCr space — should be preserved
        assert_eq!(output[3], 255);
        assert_eq!(output[0], 255);
    }

    #[test]
    fn white_not_keyed_by_green() {
        let input = vec![255, 255, 255, 255]; // white pixel
        let output = apply_no_spill(input, [0, 255, 0], 0.3, 0.2);
        // White has different CbCr than pure green
        assert!(output[3] > 0);
    }

    #[test]
    fn blue_screen_key() {
        let input = vec![0, 0, 255, 255]; // pure blue pixel
        let output = apply_no_spill(input, [0, 0, 255], 0.3, 0.2);
        assert_eq!(output[3], 0); // keyed out
    }

    #[test]
    fn near_green_gets_soft_edge() {
        // A slightly off-green pixel — should get partial alpha
        let input = vec![30, 220, 30, 255];
        let output = apply_no_spill(input, [0, 255, 0], 0.05, 0.5);
        // Should have reduced but non-zero alpha (soft edge)
        assert!(output[3] > 0);
        assert!(output[3] < 255);
    }

    // ── Spill suppression tests ────────────────────────────────────

    #[test]
    fn spill_method_none_does_not_modify_rgb() {
        // A skin-tone pixel with green spill — full RGBA preserved
        // when method is None, even with strength > 0.
        let input = vec![200, 180, 140, 255];
        let output = apply(input.clone(), [0, 255, 0], 0.05, 0.05, SpillMethod::None, 1.0);
        // Pixel sits well outside the green CbCr band — alpha untouched.
        assert_eq!(output[3], 255);
        // RGB also untouched (spill stage is a no-op when method is None).
        assert_eq!(output[0..3], input[0..3]);
    }

    #[test]
    fn spill_strength_zero_is_passthrough() {
        // Strength=0 means even with method set, RGB is unchanged.
        let input = vec![200, 220, 140, 255]; // green-cast skin
        let output = apply(input.clone(), [0, 255, 0], 0.05, 0.05, SpillMethod::ChannelClamp, 0.0);
        assert_eq!(output[0..3], input[0..3]);
    }

    #[test]
    fn channel_clamp_reduces_green_on_skin_pixel() {
        // Skin tone with green spill: G > max(R, B). ChannelClamp at
        // strength 1.0 should drop G to max(R, B) = 200.
        // Pick a pixel WELL outside the green CbCr key band so the
        // matte doesn't touch alpha — we want to isolate spill.
        let input = vec![200, 220, 140, 255];
        let output = apply(input, [0, 255, 0], 0.05, 0.05, SpillMethod::ChannelClamp, 1.0);
        // R, B untouched.
        assert_eq!(output[0], 200);
        assert_eq!(output[2], 140);
        // G clamped to max(R, B) = 200.
        assert_eq!(output[1], 200, "G should clamp to max(R,B)=200, got {}", output[1]);
        // Alpha intact.
        assert_eq!(output[3], 255);
    }

    #[test]
    fn hawk_more_aggressive_than_channel_clamp() {
        // Same skin-tone input. Hawk targets avg of others (170);
        // ChannelClamp targets max of others (200). Hawk drops G lower.
        let input = vec![200, 220, 140, 255];
        let clamp = apply(input.clone(), [0, 255, 0], 0.05, 0.05, SpillMethod::ChannelClamp, 1.0);
        let hawk = apply(input,           [0, 255, 0], 0.05, 0.05, SpillMethod::Hawk,         1.0);
        assert!(
            hawk[1] < clamp[1],
            "Hawk should drop G further than ChannelClamp: hawk={}, clamp={}",
            hawk[1], clamp[1]
        );
    }

    #[test]
    fn spill_only_affects_key_channel() {
        // Green key — only G should change. Use multiple pixels.
        let input = vec![
            200, 220, 140, 255, // skin with green spill
            100, 180, 100, 255, // greenish-grey
            50,  200, 80,  255, // dark green spill
        ];
        let original = input.clone();
        let output = apply(input, [0, 255, 0], 0.0, 0.0, SpillMethod::ChannelClamp, 1.0);
        // Pixels with no green spill (G ≤ max(R,B)) won't change.
        // For each pixel: only G may differ; R and B match input.
        for i in 0..3 {
            let off = i * 4;
            assert_eq!(output[off],     original[off],     "pixel {i} R changed");
            assert_eq!(output[off + 2], original[off + 2], "pixel {i} B changed");
        }
    }

    #[test]
    fn spill_skipped_for_keyed_out_pixels() {
        // Pure green pixel gets fully keyed out → alpha=0, RGB=0.
        // Spill stage must NOT run on this (and even if it did,
        // it'd touch a zero pixel). Asserts the early-out works.
        let input = vec![0, 255, 0, 255];
        let output = apply(input, [0, 255, 0], 0.3, 0.2, SpillMethod::ChannelClamp, 1.0);
        assert_eq!(output, vec![0, 0, 0, 0], "keyed-out pixel zeroed");
    }

    #[test]
    fn dominant_channel_picks_key() {
        assert_eq!(dominant_channel([0, 255, 0]), 1, "green key → G");
        assert_eq!(dominant_channel([0, 0, 255]), 2, "blue key → B");
        assert_eq!(dominant_channel([255, 0, 0]), 0, "red key → R");
        // Ties: first max wins (R then G then B).
        assert_eq!(dominant_channel([100, 100, 100]), 1, "all equal → G (favour green-key default)");
    }

    #[test]
    fn blue_key_clamps_blue_channel() {
        // Blue key → spill suppressor should clamp B, not G.
        let input = vec![140, 180, 220, 255]; // blue-cast skin
        let output = apply(input, [0, 0, 255], 0.05, 0.05, SpillMethod::ChannelClamp, 1.0);
        // R, G untouched.
        assert_eq!(output[0], 140);
        assert_eq!(output[1], 180);
        // B clamped to max(R, G) = 180.
        assert_eq!(output[2], 180, "B should clamp to max(R,G)=180, got {}", output[2]);
    }

    #[test]
    fn spill_strength_half_blends_proportionally() {
        // strength=0.5 → key channel moves halfway from current to target.
        // Skin: R=200, G=220, B=140. ChannelClamp target = max(R,B) = 200.
        // G goes 220 → 200 by 0.5 = 220 + 0.5*(200-220) = 210.
        let input = vec![200, 220, 140, 255];
        let output = apply(input, [0, 255, 0], 0.05, 0.05, SpillMethod::ChannelClamp, 0.5);
        assert_eq!(output[1], 210, "G should land at midpoint, got {}", output[1]);
    }
}
