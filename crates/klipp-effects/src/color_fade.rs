//! Per-pixel mix toward a target colour.
//!
//! Applied as part of the per-clip effects chain to drive a colour
//! fade region (fade-from-black, fade-to-white, etc.). The engine
//! patches `progress` per-frame from clip-time + the effect's
//! `duration_secs` + `mode`, mirroring how `BuiltinEffect::Opacity`
//! gets its `value` patched. See `BuiltinEffect::ColorFade`.
//!
//! Convention: `progress = 0.0` → full target colour, `progress = 1.0`
//! → full source unmodified. Linear interpolation in sRGB-u8 space —
//! the effect runs after the linear-to-u8 bridge alongside other
//! sRGB-domain effects (sharpen, chroma key, etc.).

/// Mix `rgba` toward `color` by `1.0 - progress`. `progress` is clamped
/// to `[0.0, 1.0]`; out-of-range values would be a calling-side bug
/// but the clamp keeps the inner loop branch-free.
pub fn apply(mut rgba: Vec<u8>, color: [u8; 4], progress: f32) -> Vec<u8> {
    let p = progress.clamp(0.0, 1.0);
    if p >= 0.9999 {
        // Fully source — bypass.
        return rgba;
    }
    let inv = 1.0 - p;
    let cr = color[0] as f32;
    let cg = color[1] as f32;
    let cb = color[2] as f32;
    let ca = color[3] as f32;
    for px in rgba.chunks_exact_mut(4) {
        px[0] = ((px[0] as f32) * p + cr * inv).round().clamp(0.0, 255.0) as u8;
        px[1] = ((px[1] as f32) * p + cg * inv).round().clamp(0.0, 255.0) as u8;
        px[2] = ((px[2] as f32) * p + cb * inv).round().clamp(0.0, 255.0) as u8;
        // Alpha mixes from `color.a` at progress=0 toward source alpha
        // at progress=1. With color.a = 255 (typical "fade from black"),
        // this means the fade region fully covers below-track content
        // at the start of the fade — matching what every NLE does for
        // fade-from-black.
        px[3] = ((px[3] as f32) * p + ca * inv).round().clamp(0.0, 255.0) as u8;
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(rgba: &[u8], i: usize) -> [u8; 4] {
        [rgba[i*4], rgba[i*4+1], rgba[i*4+2], rgba[i*4+3]]
    }

    #[test]
    fn progress_zero_renders_full_color() {
        // 2-pixel buffer of mid-grey
        let src = vec![128, 128, 128, 255,  128, 128, 128, 255];
        let out = apply(src, [255, 0, 0, 255], 0.0);
        assert_eq!(px(&out, 0), [255, 0, 0, 255]);
        assert_eq!(px(&out, 1), [255, 0, 0, 255]);
    }

    #[test]
    fn progress_one_passes_through() {
        let src = vec![100, 150, 200, 250];
        let out = apply(src.clone(), [0, 0, 0, 0], 1.0);
        assert_eq!(out, src);
    }

    #[test]
    fn progress_half_mixes_evenly() {
        let src = vec![200, 0, 0, 255];           // red source
        let out = apply(src, [0, 0, 200, 255], 0.5); // mix toward blue
        // 200*0.5 + 0*0.5 = 100  (R)
        // 0*0.5  + 0*0.5 = 0    (G)
        // 0*0.5  + 200*0.5 = 100 (B)
        // 255*0.5 + 255*0.5 = 255
        assert_eq!(px(&out, 0), [100, 0, 100, 255]);
    }

    #[test]
    fn alpha_lerps_toward_color_alpha_at_progress_zero() {
        // Source has zero alpha; colour wants full coverage.
        let src = vec![100, 100, 100, 0];
        let out = apply(src, [0, 0, 0, 255], 0.0);
        assert_eq!(out[3], 255, "fade region should cover below-track");
    }

    #[test]
    fn negative_progress_clamps_to_zero() {
        let src = vec![200, 200, 200, 255];
        let out = apply(src, [0, 0, 0, 255], -0.5);
        // Negative clamps to 0 → full colour.
        assert_eq!(px(&out, 0), [0, 0, 0, 255]);
    }
}
