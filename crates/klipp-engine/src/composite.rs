use klipp_core::BlendMode;

pub struct CompositeLayer {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub opacity: f32,
    pub blend_mode: BlendMode,
}

/// Composite multiple layers into a single RGBA output.
pub fn composite_layers(layers: &[CompositeLayer], out_width: u32, out_height: u32) -> Vec<u8> {
    let pixel_count = (out_width * out_height) as usize;
    let mut output = vec![0u8; pixel_count * 4];

    for layer in layers {
        let src = if layer.width == out_width && layer.height == out_height {
            &layer.rgba
        } else {
            &scale_rgba(&layer.rgba, layer.width, layer.height, out_width, out_height)
        };
        blend_layer(&mut output, src, layer.opacity, layer.blend_mode);
    }
    output
}

/// Blend a source layer onto the destination using the specified blend mode.
fn blend_layer(dst: &mut [u8], src: &[u8], opacity: f32, mode: BlendMode) {
    if src.len() != dst.len() {
        return;
    }
    for i in (0..dst.len()).step_by(4) {
        let sa = (src[i + 3] as f32 / 255.0) * opacity;
        if sa < 0.001 { continue; }
        let da = dst[i + 3] as f32 / 255.0;

        // Normalize to 0..1
        let sr = src[i] as f32 / 255.0;
        let sg = src[i + 1] as f32 / 255.0;
        let sb = src[i + 2] as f32 / 255.0;
        let dr = dst[i] as f32 / 255.0;
        let dg = dst[i + 1] as f32 / 255.0;
        let db = dst[i + 2] as f32 / 255.0;

        // Apply blend mode to get blended RGB
        let (br, bg, bb) = match mode {
            BlendMode::Normal => (sr, sg, sb),
            BlendMode::Add => ((dr + sr).min(1.0), (dg + sg).min(1.0), (db + sb).min(1.0)),
            BlendMode::Multiply => (dr * sr, dg * sg, db * sb),
            BlendMode::Screen => (1.0 - (1.0 - dr) * (1.0 - sr), 1.0 - (1.0 - dg) * (1.0 - sg), 1.0 - (1.0 - db) * (1.0 - sb)),
            BlendMode::Overlay => (overlay_ch(dr, sr), overlay_ch(dg, sg), overlay_ch(db, sb)),
            BlendMode::SoftLight => (soft_light_ch(dr, sr), soft_light_ch(dg, sg), soft_light_ch(db, sb)),
            BlendMode::HardLight => (overlay_ch(sr, dr), overlay_ch(sg, dg), overlay_ch(sb, db)),
            BlendMode::Difference => ((dr - sr).abs(), (dg - sg).abs(), (db - sb).abs()),
            BlendMode::Exclusion => (dr + sr - 2.0 * dr * sr, dg + sg - 2.0 * dg * sg, db + sb - 2.0 * db * sb),
            BlendMode::ColorDodge => (dodge_ch(dr, sr), dodge_ch(dg, sg), dodge_ch(db, sb)),
            BlendMode::ColorBurn => (burn_ch(dr, sr), burn_ch(dg, sg), burn_ch(db, sb)),
            BlendMode::Darken => (dr.min(sr), dg.min(sg), db.min(sb)),
            BlendMode::Lighten => (dr.max(sr), dg.max(sg), db.max(sb)),
        };

        // Alpha compositing: blend result over destination
        let out_a = sa + da * (1.0 - sa);
        if out_a > 0.0 {
            dst[i] = ((br * sa + dr * da * (1.0 - sa)) / out_a * 255.0).clamp(0.0, 255.0) as u8;
            dst[i + 1] = ((bg * sa + dg * da * (1.0 - sa)) / out_a * 255.0).clamp(0.0, 255.0) as u8;
            dst[i + 2] = ((bb * sa + db * da * (1.0 - sa)) / out_a * 255.0).clamp(0.0, 255.0) as u8;
            dst[i + 3] = (out_a * 255.0) as u8;
        }
    }
}

#[inline]
fn overlay_ch(base: f32, blend: f32) -> f32 {
    if base < 0.5 {
        2.0 * base * blend
    } else {
        1.0 - 2.0 * (1.0 - base) * (1.0 - blend)
    }
}

#[inline]
fn soft_light_ch(base: f32, blend: f32) -> f32 {
    if blend < 0.5 {
        base - (1.0 - 2.0 * blend) * base * (1.0 - base)
    } else {
        let d = if base < 0.25 {
            ((16.0 * base - 12.0) * base + 4.0) * base
        } else {
            base.sqrt()
        };
        base + (2.0 * blend - 1.0) * (d - base)
    }
}

#[inline]
fn dodge_ch(base: f32, blend: f32) -> f32 {
    if blend >= 1.0 { 1.0 } else { (base / (1.0 - blend)).min(1.0) }
}

#[inline]
fn burn_ch(base: f32, blend: f32) -> f32 {
    if blend <= 0.0 { 0.0 } else { (1.0 - (1.0 - base) / blend).max(0.0) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::BlendMode;

    /// Make a single-pixel RGBA buffer.
    fn pixel(r: u8, g: u8, b: u8, a: u8) -> Vec<u8> {
        vec![r, g, b, a]
    }

    /// Run blend_layer on single-pixel src over dst, return resulting RGBA.
    fn blend_one(dst_rgba: [u8; 4], src_rgba: [u8; 4], opacity: f32, mode: BlendMode) -> [u8; 4] {
        let mut dst = vec![dst_rgba[0], dst_rgba[1], dst_rgba[2], dst_rgba[3]];
        let src = vec![src_rgba[0], src_rgba[1], src_rgba[2], src_rgba[3]];
        blend_layer(&mut dst, &src, opacity, mode);
        [dst[0], dst[1], dst[2], dst[3]]
    }

    /// Assert pixel values are within tolerance (for floating-point rounding).
    fn assert_pixel(actual: [u8; 4], expected: [u8; 4], mode: &str, case: &str) {
        for ch in 0..4 {
            let diff = (actual[ch] as i16 - expected[ch] as i16).unsigned_abs();
            assert!(
                diff <= 1,
                "{mode} / {case}: channel {ch} expected {} got {} (diff {diff})",
                expected[ch], actual[ch]
            );
        }
    }

    // Black = [0,0,0,255], White = [255,255,255,255], Gray50 = [128,128,128,255]
    const BLACK: [u8; 4] = [0, 0, 0, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const GRAY50: [u8; 4] = [128, 128, 128, 255];

    // ---- Normal ----
    #[test]
    fn normal_black_over_white() {
        let r = blend_one(WHITE, BLACK, 1.0, BlendMode::Normal);
        assert_pixel(r, BLACK, "Normal", "black over white");
    }

    #[test]
    fn normal_white_over_black() {
        let r = blend_one(BLACK, WHITE, 1.0, BlendMode::Normal);
        assert_pixel(r, WHITE, "Normal", "white over black");
    }

    #[test]
    fn normal_gray_over_gray() {
        let r = blend_one(GRAY50, GRAY50, 1.0, BlendMode::Normal);
        assert_pixel(r, GRAY50, "Normal", "gray over gray");
    }

    #[test]
    fn normal_half_opacity() {
        // Black at 50% over white => ~128
        let r = blend_one(WHITE, BLACK, 0.5, BlendMode::Normal);
        assert_pixel(r, [128, 128, 128, 255], "Normal", "half opacity");
    }

    // ---- Add ----
    #[test]
    fn add_black_over_white() {
        // 0+1 clamped to 1 => white
        let r = blend_one(WHITE, BLACK, 1.0, BlendMode::Add);
        assert_pixel(r, WHITE, "Add", "black over white");
    }

    #[test]
    fn add_white_over_black() {
        let r = blend_one(BLACK, WHITE, 1.0, BlendMode::Add);
        assert_pixel(r, WHITE, "Add", "white over black");
    }

    #[test]
    fn add_gray_over_gray() {
        // 0.502 + 0.502 = 1.004 clamped to 1.0 => 255
        let r = blend_one(GRAY50, GRAY50, 1.0, BlendMode::Add);
        assert_pixel(r, WHITE, "Add", "gray over gray");
    }

    #[test]
    fn add_half_opacity() {
        // Blend result: (0+1)=1.0 clamped, then composited at 50%
        // out = 1.0*0.5 + 1.0*1.0*0.5 = 1.0 => white
        let r = blend_one(WHITE, WHITE, 0.5, BlendMode::Add);
        assert_pixel(r, WHITE, "Add", "half opacity");
    }

    // ---- Multiply ----
    #[test]
    fn multiply_black_over_white() {
        // 1.0 * 0.0 = 0.0 => black
        let r = blend_one(WHITE, BLACK, 1.0, BlendMode::Multiply);
        assert_pixel(r, [0, 0, 0, 255], "Multiply", "black over white");
    }

    #[test]
    fn multiply_white_over_black() {
        // 0.0 * 1.0 = 0.0 => black
        let r = blend_one(BLACK, WHITE, 1.0, BlendMode::Multiply);
        assert_pixel(r, [0, 0, 0, 255], "Multiply", "white over black");
    }

    #[test]
    fn multiply_white_over_white() {
        // 1.0 * 1.0 = 1.0 => white
        let r = blend_one(WHITE, WHITE, 1.0, BlendMode::Multiply);
        assert_pixel(r, WHITE, "Multiply", "white over white");
    }

    #[test]
    fn multiply_gray_over_gray() {
        // 0.502 * 0.502 ~ 0.252 => ~64
        let r = blend_one(GRAY50, GRAY50, 1.0, BlendMode::Multiply);
        assert_pixel(r, [64, 64, 64, 255], "Multiply", "gray over gray");
    }

    #[test]
    fn multiply_half_opacity() {
        // blend=0*1=0, composite: 0*0.5 + 1.0*1.0*0.5 = 0.5 => 128
        let r = blend_one(WHITE, BLACK, 0.5, BlendMode::Multiply);
        assert_pixel(r, [128, 128, 128, 255], "Multiply", "half opacity");
    }

    // ---- Screen ----
    #[test]
    fn screen_black_over_white() {
        // 1-(1-1)(1-0)=1 => white
        let r = blend_one(WHITE, BLACK, 1.0, BlendMode::Screen);
        assert_pixel(r, WHITE, "Screen", "black over white");
    }

    #[test]
    fn screen_white_over_black() {
        // 1-(1-0)(1-1)=1 => white
        let r = blend_one(BLACK, WHITE, 1.0, BlendMode::Screen);
        assert_pixel(r, WHITE, "Screen", "white over black");
    }

    #[test]
    fn screen_black_over_black() {
        let r = blend_one(BLACK, BLACK, 1.0, BlendMode::Screen);
        assert_pixel(r, BLACK, "Screen", "black over black");
    }

    #[test]
    fn screen_gray_over_gray() {
        // 1-(1-0.502)(1-0.502) = 1 - 0.498*0.498 ~ 0.752 => ~192
        let r = blend_one(GRAY50, GRAY50, 1.0, BlendMode::Screen);
        assert_pixel(r, [192, 192, 192, 255], "Screen", "gray over gray");
    }

    #[test]
    fn screen_half_opacity() {
        let r = blend_one(BLACK, WHITE, 0.5, BlendMode::Screen);
        // blend=1.0, composite: 1.0*0.5 + 0*1*0.5 = 0.5 => 128
        assert_pixel(r, [128, 128, 128, 255], "Screen", "half opacity");
    }

    // ---- Overlay ----
    #[test]
    fn overlay_black_over_white() {
        // base=1.0 >=0.5 => 1-2*(1-1)*(1-0)=1 => white
        let r = blend_one(WHITE, BLACK, 1.0, BlendMode::Overlay);
        assert_pixel(r, WHITE, "Overlay", "black over white");
    }

    #[test]
    fn overlay_white_over_black() {
        // base=0 <0.5 => 2*0*1=0 => black
        let r = blend_one(BLACK, WHITE, 1.0, BlendMode::Overlay);
        assert_pixel(r, BLACK, "Overlay", "white over black");
    }

    #[test]
    fn overlay_gray_over_gray() {
        // base=0.502 >=0.5 => 1-2*(1-0.502)*(1-0.502) ~ 1-2*0.248=0.504 => ~128
        let r = blend_one(GRAY50, GRAY50, 1.0, BlendMode::Overlay);
        assert_pixel(r, [128, 128, 128, 255], "Overlay", "gray over gray");
    }

    #[test]
    fn overlay_half_opacity() {
        let r = blend_one(WHITE, BLACK, 0.5, BlendMode::Overlay);
        // blend=1.0 (overlay: base=1>=0.5 => 1-2*0*1=1), composite at 50%: 1*0.5+1*1*0.5=1 => 255
        assert_pixel(r, WHITE, "Overlay", "half opacity");
    }

    // ---- SoftLight ----
    #[test]
    fn soft_light_black_over_white() {
        // blend=0<0.5 => base - (1-0)*base*(1-base) = 1 - 1*1*0 = 1 => white
        let r = blend_one(WHITE, BLACK, 1.0, BlendMode::SoftLight);
        assert_pixel(r, WHITE, "SoftLight", "black over white");
    }

    #[test]
    fn soft_light_white_over_black() {
        // blend=1>=0.5 => d=sqrt(0)=0, base+(2*1-1)*(0-0)=0 => black
        let r = blend_one(BLACK, WHITE, 1.0, BlendMode::SoftLight);
        assert_pixel(r, BLACK, "SoftLight", "white over black");
    }

    #[test]
    fn soft_light_gray_over_gray() {
        // blend=0.502>=0.5, d=sqrt(0.502)~0.7085
        // result=0.502+(2*0.502-1)*(0.7085-0.502) = 0.502+0.004*0.2065 ~ 0.503
        let r = blend_one(GRAY50, GRAY50, 1.0, BlendMode::SoftLight);
        assert_pixel(r, [128, 128, 128, 255], "SoftLight", "gray over gray");
    }

    #[test]
    fn soft_light_half_opacity() {
        let r = blend_one(WHITE, GRAY50, 0.5, BlendMode::SoftLight);
        assert_pixel(r, WHITE, "SoftLight", "half opacity");
    }

    // ---- HardLight ----
    #[test]
    fn hard_light_black_over_white() {
        // HardLight = overlay with args swapped: overlay_ch(src=0, dst=1)
        // src<0.5 => 2*0*1=0 => black
        let r = blend_one(WHITE, BLACK, 1.0, BlendMode::HardLight);
        assert_pixel(r, BLACK, "HardLight", "black over white");
    }

    #[test]
    fn hard_light_white_over_black() {
        // overlay_ch(src=1, dst=0): src>=0.5 => 1-2*(1-1)*(1-0)=1 => white
        let r = blend_one(BLACK, WHITE, 1.0, BlendMode::HardLight);
        assert_pixel(r, WHITE, "HardLight", "white over black");
    }

    #[test]
    fn hard_light_gray_over_gray() {
        // overlay_ch(0.502, 0.502): 0.502>=0.5 => 1-2*0.498*0.498 ~ 0.504 => ~128
        let r = blend_one(GRAY50, GRAY50, 1.0, BlendMode::HardLight);
        assert_pixel(r, [128, 128, 128, 255], "HardLight", "gray over gray");
    }

    #[test]
    fn hard_light_half_opacity() {
        let r = blend_one(WHITE, BLACK, 0.5, BlendMode::HardLight);
        // blend=0, composite: 0*0.5 + 1*1*0.5 = 0.5 => 128
        assert_pixel(r, [128, 128, 128, 255], "HardLight", "half opacity");
    }

    // ---- Difference ----
    #[test]
    fn difference_black_over_white() {
        // |1-0|=1 => white
        let r = blend_one(WHITE, BLACK, 1.0, BlendMode::Difference);
        assert_pixel(r, WHITE, "Difference", "black over white");
    }

    #[test]
    fn difference_white_over_black() {
        // |0-1|=1 => white
        let r = blend_one(BLACK, WHITE, 1.0, BlendMode::Difference);
        assert_pixel(r, WHITE, "Difference", "white over black");
    }

    #[test]
    fn difference_same_color() {
        // |x - x| = 0 => black
        let r = blend_one(GRAY50, GRAY50, 1.0, BlendMode::Difference);
        assert_pixel(r, [0, 0, 0, 255], "Difference", "same color");
    }

    #[test]
    fn difference_half_opacity() {
        let r = blend_one(WHITE, BLACK, 0.5, BlendMode::Difference);
        // blend=|1-0|=1, composite: 1*0.5 + 1*1*0.5 = 1 => 255
        assert_pixel(r, WHITE, "Difference", "half opacity");
    }

    // ---- Exclusion ----
    #[test]
    fn exclusion_black_over_white() {
        // 1+0 - 2*1*0 = 1 => white
        let r = blend_one(WHITE, BLACK, 1.0, BlendMode::Exclusion);
        assert_pixel(r, WHITE, "Exclusion", "black over white");
    }

    #[test]
    fn exclusion_white_over_black() {
        // 0+1 - 2*0*1 = 1 => white
        let r = blend_one(BLACK, WHITE, 1.0, BlendMode::Exclusion);
        assert_pixel(r, WHITE, "Exclusion", "white over black");
    }

    #[test]
    fn exclusion_gray_over_gray() {
        // 0.502+0.502 - 2*0.502*0.502 ~ 1.004 - 0.504 = 0.500 => ~128
        let r = blend_one(GRAY50, GRAY50, 1.0, BlendMode::Exclusion);
        assert_pixel(r, [128, 128, 128, 255], "Exclusion", "gray over gray");
    }

    #[test]
    fn exclusion_same_white() {
        // 1+1-2*1*1 = 0 => black
        let r = blend_one(WHITE, WHITE, 1.0, BlendMode::Exclusion);
        assert_pixel(r, [0, 0, 0, 255], "Exclusion", "white over white");
    }

    #[test]
    fn exclusion_half_opacity() {
        let r = blend_one(BLACK, WHITE, 0.5, BlendMode::Exclusion);
        // blend=0+1-0=1, composite: 1*0.5 + 0*1*0.5 = 0.5 => 128
        assert_pixel(r, [128, 128, 128, 255], "Exclusion", "half opacity");
    }

    // ---- ColorDodge ----
    #[test]
    fn dodge_black_over_white() {
        // base=1, blend=0 => 1/(1-0)=1 => white
        let r = blend_one(WHITE, BLACK, 1.0, BlendMode::ColorDodge);
        assert_pixel(r, WHITE, "ColorDodge", "black over white");
    }

    #[test]
    fn dodge_white_over_black() {
        // base=0, blend=1 => blend>=1 => 1 => white (special case)
        let r = blend_one(BLACK, WHITE, 1.0, BlendMode::ColorDodge);
        assert_pixel(r, WHITE, "ColorDodge", "white over black");
    }

    #[test]
    fn dodge_black_over_black() {
        // base=0, blend=0 => 0/(1-0)=0 => black
        let r = blend_one(BLACK, BLACK, 1.0, BlendMode::ColorDodge);
        assert_pixel(r, BLACK, "ColorDodge", "black over black");
    }

    #[test]
    fn dodge_gray_over_gray() {
        // 0.502/(1-0.502) = 0.502/0.498 ~ 1.008 clamped to 1 => 255
        let r = blend_one(GRAY50, GRAY50, 1.0, BlendMode::ColorDodge);
        assert_pixel(r, WHITE, "ColorDodge", "gray over gray");
    }

    #[test]
    fn dodge_divide_by_zero() {
        // blend=1.0 (white) => special case returns 1.0, no divide-by-zero
        let r = blend_one(GRAY50, WHITE, 1.0, BlendMode::ColorDodge);
        assert_pixel(r, WHITE, "ColorDodge", "divide-by-zero protection");
    }

    #[test]
    fn dodge_half_opacity() {
        let r = blend_one(GRAY50, BLACK, 0.5, BlendMode::ColorDodge);
        // blend result: 0.502/(1-0)=0.502, composite at 50%: 0.502*0.5+0.502*1*0.5=0.502 => 128
        assert_pixel(r, GRAY50, "ColorDodge", "half opacity");
    }

    // ---- ColorBurn ----
    #[test]
    fn burn_black_over_white() {
        // base=1, blend=0 => blend<=0 => 0 (special case) => black
        let r = blend_one(WHITE, BLACK, 1.0, BlendMode::ColorBurn);
        assert_pixel(r, BLACK, "ColorBurn", "black over white");
    }

    #[test]
    fn burn_white_over_black() {
        // base=0, blend=1 => 1-(1-0)/1=0 => black
        let r = blend_one(BLACK, WHITE, 1.0, BlendMode::ColorBurn);
        assert_pixel(r, BLACK, "ColorBurn", "white over black");
    }

    #[test]
    fn burn_white_over_white() {
        // 1-(1-1)/1 = 1 => white
        let r = blend_one(WHITE, WHITE, 1.0, BlendMode::ColorBurn);
        assert_pixel(r, WHITE, "ColorBurn", "white over white");
    }

    #[test]
    fn burn_gray_over_gray() {
        // 1-(1-0.502)/0.502 = 1-0.498/0.502 ~ 1-0.992 = 0.008 => ~2
        let r = blend_one(GRAY50, GRAY50, 1.0, BlendMode::ColorBurn);
        assert_pixel(r, [2, 2, 2, 255], "ColorBurn", "gray over gray");
    }

    #[test]
    fn burn_divide_by_zero() {
        // blend=0 (black) => special case returns 0, no divide-by-zero
        let r = blend_one(GRAY50, BLACK, 1.0, BlendMode::ColorBurn);
        assert_pixel(r, BLACK, "ColorBurn", "divide-by-zero protection");
    }

    #[test]
    fn burn_half_opacity() {
        let r = blend_one(WHITE, WHITE, 0.5, BlendMode::ColorBurn);
        // blend=1-(1-1)/1=1, composite: 1*0.5+1*1*0.5=1 => 255
        assert_pixel(r, WHITE, "ColorBurn", "half opacity");
    }

    // ---- Darken ----
    #[test]
    fn darken_black_over_white() {
        // min(1,0) = 0 => black
        let r = blend_one(WHITE, BLACK, 1.0, BlendMode::Darken);
        assert_pixel(r, BLACK, "Darken", "black over white");
    }

    #[test]
    fn darken_white_over_black() {
        // min(0,1) = 0 => black
        let r = blend_one(BLACK, WHITE, 1.0, BlendMode::Darken);
        assert_pixel(r, BLACK, "Darken", "white over black");
    }

    #[test]
    fn darken_gray_over_gray() {
        let r = blend_one(GRAY50, GRAY50, 1.0, BlendMode::Darken);
        assert_pixel(r, GRAY50, "Darken", "gray over gray");
    }

    #[test]
    fn darken_half_opacity() {
        let r = blend_one(WHITE, BLACK, 0.5, BlendMode::Darken);
        // blend=0, composite: 0*0.5+1*1*0.5=0.5 => 128
        assert_pixel(r, [128, 128, 128, 255], "Darken", "half opacity");
    }

    // ---- Lighten ----
    #[test]
    fn lighten_black_over_white() {
        // max(1,0) = 1 => white
        let r = blend_one(WHITE, BLACK, 1.0, BlendMode::Lighten);
        assert_pixel(r, WHITE, "Lighten", "black over white");
    }

    #[test]
    fn lighten_white_over_black() {
        // max(0,1) = 1 => white
        let r = blend_one(BLACK, WHITE, 1.0, BlendMode::Lighten);
        assert_pixel(r, WHITE, "Lighten", "white over black");
    }

    #[test]
    fn lighten_gray_over_gray() {
        let r = blend_one(GRAY50, GRAY50, 1.0, BlendMode::Lighten);
        assert_pixel(r, GRAY50, "Lighten", "gray over gray");
    }

    #[test]
    fn lighten_half_opacity() {
        let r = blend_one(BLACK, WHITE, 0.5, BlendMode::Lighten);
        // blend=max(0,1)=1, composite: 1*0.5+0*1*0.5=0.5 => 128
        assert_pixel(r, [128, 128, 128, 255], "Lighten", "half opacity");
    }

    // ---- Cross-cutting tests ----

    #[test]
    fn zero_opacity_is_noop() {
        for mode in [
            BlendMode::Normal, BlendMode::Add, BlendMode::Multiply,
            BlendMode::Screen, BlendMode::Overlay, BlendMode::SoftLight,
            BlendMode::HardLight, BlendMode::Difference, BlendMode::Exclusion,
            BlendMode::ColorDodge, BlendMode::ColorBurn,
            BlendMode::Darken, BlendMode::Lighten,
        ] {
            let r = blend_one(GRAY50, WHITE, 0.0, mode);
            assert_pixel(r, GRAY50, &format!("{:?}", mode), "zero opacity noop");
        }
    }

    #[test]
    fn mismatched_buffer_lengths_is_noop() {
        let mut dst = vec![128u8; 4];
        let src = vec![255u8; 8]; // different length
        blend_layer(&mut dst, &src, 1.0, BlendMode::Normal);
        assert_eq!(dst, vec![128u8; 4], "mismatched lengths should be no-op");
    }

    #[test]
    fn transparent_src_is_noop() {
        // src alpha=0 => sa < 0.001 => skip
        let r = blend_one(GRAY50, [255, 0, 0, 0], 1.0, BlendMode::Normal);
        assert_pixel(r, GRAY50, "Normal", "transparent src");
    }

    #[test]
    fn composite_layers_basic() {
        let base = CompositeLayer {
            rgba: pixel(255, 0, 0, 255),
            width: 1, height: 1,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
        };
        let top = CompositeLayer {
            rgba: pixel(0, 255, 0, 255),
            width: 1, height: 1,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
        };
        let out = composite_layers(&[base, top], 1, 1);
        assert_eq!(out, vec![0, 255, 0, 255], "top layer should win in Normal mode");
    }

    #[test]
    fn multi_pixel_blend() {
        // 2-pixel buffer to verify stride iteration
        let mut dst = vec![255, 255, 255, 255, 0, 0, 0, 255];
        let src = vec![0, 0, 0, 255, 255, 255, 255, 255];
        blend_layer(&mut dst, &src, 1.0, BlendMode::Normal);
        assert_eq!(&dst[0..4], &[0, 0, 0, 255]);
        assert_eq!(&dst[4..8], &[255, 255, 255, 255]);
    }
}

fn scale_rgba(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    use image::{ImageBuffer, Rgba};
    let img = match ImageBuffer::<Rgba<u8>, _>::from_raw(src_w, src_h, src.to_vec()) {
        Some(img) => img,
        None => return vec![0u8; (dst_w * dst_h * 4) as usize],
    };
    let scaled = image::imageops::resize(&img, dst_w, dst_h, image::imageops::FilterType::Triangle);
    scaled.into_raw()
}
