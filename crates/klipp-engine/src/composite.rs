use klipp_core::BlendMode;
use rayon::prelude::*;

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
/// Public to support incremental compositing in the viewer's adjustment layer
/// loop, which needs to intercept at adjustment layer boundaries.
///
/// Parallelized across rayon threads in pixel-aligned chunks. Each chunk is
/// fully independent (no cross-pixel dependencies). Combined with AVX2
/// auto-vectorization from `target-cpu=native`, this produces ~N_cores × ~5×
/// throughput improvement over the scalar single-threaded baseline.
pub fn blend_layer(dst: &mut [u8], src: &[u8], opacity: f32, mode: BlendMode) {
    if src.len() != dst.len() {
        return;
    }

    // Minimum chunk size of 4096 pixels (16 KB) avoids rayon scheduling overhead
    // from dominating at small resolutions (proxy media, thumbnails). Buffers
    // smaller than this run on a single thread.
    const MIN_CHUNK_PIXELS: usize = 4096;
    const MIN_CHUNK_BYTES: usize = MIN_CHUNK_PIXELS * 4;

    if dst.len() <= MIN_CHUNK_BYTES {
        blend_chunk(dst, src, opacity, mode);
        return;
    }

    // Aim for one chunk per rayon thread, but respect the minimum.
    // Round down to a pixel boundary (multiple of 4) so chunks never split an RGBA pixel.
    let threads = rayon::current_num_threads().max(1);
    let chunk_bytes = ((dst.len() / threads).max(MIN_CHUNK_BYTES) / 4) * 4;

    dst.par_chunks_mut(chunk_bytes)
        .zip(src.par_chunks(chunk_bytes))
        .for_each(|(dst_chunk, src_chunk)| {
            blend_chunk(dst_chunk, src_chunk, opacity, mode);
        });
}

/// Inner blend loop — processes one chunk of pixels.
/// Extracted from `blend_layer` so LLVM can auto-vectorize the stride-4 loop
/// with AVX2 instructions (enabled via `target-cpu=native` rustflag).
#[inline]
fn blend_chunk(dst: &mut [u8], src: &[u8], opacity: f32, mode: BlendMode) {
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

    // ═══════════════════════════════════════════════════════════════════
    // Viewer per-clip effects regression tests
    //
    // These exercise the same math the viewer uses: apply effects per-clip
    // via klipp_effects::apply_effects(), then composite via composite_layers().
    // They don't require the full engine — just the public effect and composite APIs.
    // ═══════════════════════════════════════════════════════════════════

    fn solid_frame(r: u8, g: u8, b: u8, a: u8, w: u32, h: u32) -> Vec<u8> {
        vec![r, g, b, a].repeat((w * h) as usize)
    }

    /// Viewer flow: V1 graded + V2 graded → composite.
    /// V1 = solid red + grayscale, V2 = solid blue + half brightness.
    /// Verifies each clip's effects run independently before compositing.
    #[test]
    fn viewer_per_clip_effects_two_tracks() {
        let w = 4u32;
        let h = 4u32;

        // V1: solid red → grayscale via HSVAdjust(sat=0)
        let v1_raw = solid_frame(255, 0, 0, 255, w, h);
        let v1_graded = klipp_effects::apply_effects(
            &[klipp_effects::BuiltinEffect::HSVAdjust { hue_shift: 0.0, saturation: 0.0, value: 1.0 }],
            v1_raw, w, h,
        );
        // V1 should now be grayscale: R≈G≈B
        assert!((v1_graded[0] as i32 - v1_graded[1] as i32).abs() <= 2,
            "V1 not grayscale: R={} G={}", v1_graded[0], v1_graded[1]);

        // V2: solid blue → half brightness via HSVAdjust(value=0.5)
        let v2_raw = solid_frame(0, 0, 255, 255, w, h);
        let v2_graded = klipp_effects::apply_effects(
            &[klipp_effects::BuiltinEffect::HSVAdjust { hue_shift: 0.0, saturation: 1.0, value: 0.5 }],
            v2_raw, w, h,
        );
        // V2 should be half-brightness blue: B≈128
        assert!((v2_graded[2] as i32 - 128).abs() < 10,
            "V2 not half-bright blue: B={}", v2_graded[2]);

        // Composite: V2 opaque over V1 → should show V2's graded result
        let result = composite_layers(&[
            CompositeLayer { rgba: v1_graded, width: w, height: h, opacity: 1.0, blend_mode: BlendMode::Normal },
            CompositeLayer { rgba: v2_graded, width: w, height: h, opacity: 1.0, blend_mode: BlendMode::Normal },
        ], w, h);
        // Topmost opaque layer wins: result ≈ half-bright blue
        assert!(result[0] < 5, "R should be ≈0, got {}", result[0]);
        assert!((result[2] as i32 - 128).abs() < 10, "B should be ≈128, got {}", result[2]);
    }

    /// Semitransparent V2 reveals V1's graded frame underneath.
    /// V1 = red + grayscale → (76,76,76). V2 = green at 50% opacity.
    /// If V1's effects didn't apply (the old bug), V1 would be raw red
    /// and the blend would have R >> B. With the fix, R ≈ B (grayscale).
    #[test]
    fn viewer_v1_grayscale_visible_through_semitransparent_overlay() {
        let w = 4u32;
        let h = 4u32;

        let v1_graded = klipp_effects::apply_effects(
            &[klipp_effects::BuiltinEffect::HSVAdjust { hue_shift: 0.0, saturation: 0.0, value: 1.0 }],
            solid_frame(255, 0, 0, 255, w, h), w, h,
        );
        let v2_raw = solid_frame(0, 255, 0, 255, w, h); // green, no effects

        let result = composite_layers(&[
            CompositeLayer { rgba: v1_graded, width: w, height: h, opacity: 1.0, blend_mode: BlendMode::Normal },
            CompositeLayer { rgba: v2_raw, width: w, height: h, opacity: 0.5, blend_mode: BlendMode::Normal },
        ], w, h);

        // V1 grayscale ≈ (76,76,76) blended with 50% green → R ≈ B (grayscale property preserved)
        let r = result[0] as i32;
        let b = result[2] as i32;
        assert!((r - b).abs() < 5,
            "V1 should be grayscale (R≈B) through overlay, but R={r} B={b}");
        let g = result[1] as i32;
        assert!(g > r + 30, "Green overlay should contribute: G={g} R={r}");
    }

    /// Three tracks — each with its own effect applied before compositing.
    #[test]
    fn viewer_three_tracks_independent_effects() {
        let w = 4u32;
        let h = 4u32;

        // V1: red → grayscale
        let v1 = klipp_effects::apply_effects(
            &[klipp_effects::BuiltinEffect::HSVAdjust { hue_shift: 0.0, saturation: 0.0, value: 1.0 }],
            solid_frame(255, 0, 0, 255, w, h), w, h,
        );
        // V2: green → half brightness
        let v2 = klipp_effects::apply_effects(
            &[klipp_effects::BuiltinEffect::HSVAdjust { hue_shift: 0.0, saturation: 1.0, value: 0.5 }],
            solid_frame(0, 255, 0, 255, w, h), w, h,
        );
        // V3: blue → no effect (passthrough)
        let v3 = solid_frame(0, 0, 255, 255, w, h);

        // Stack with 33% opacity each so all three contribute
        let result = composite_layers(&[
            CompositeLayer { rgba: v1, width: w, height: h, opacity: 0.5, blend_mode: BlendMode::Normal },
            CompositeLayer { rgba: v2, width: w, height: h, opacity: 0.5, blend_mode: BlendMode::Normal },
            CompositeLayer { rgba: v3, width: w, height: h, opacity: 0.5, blend_mode: BlendMode::Normal },
        ], w, h);

        // All three should contribute — result should have all three channels nonzero
        assert!(result[0] > 5, "R should be nonzero from grayscale V1: {}", result[0]);
        assert!(result[1] > 5, "G should be nonzero from V2 green: {}", result[1]);
        assert!(result[2] > 5, "B should be nonzero from V3 blue: {}", result[2]);
    }

    /// No-effects passthrough: clip without effects produces raw output.
    #[test]
    fn viewer_no_effects_passthrough() {
        let w = 4u32;
        let h = 4u32;
        let raw = solid_frame(100, 150, 200, 255, w, h);
        let graded = klipp_effects::apply_effects(&[], raw.clone(), w, h);
        assert_eq!(graded, raw, "empty effects should pass through unchanged");
    }

    // ── Parallel blend_layer regression tests ────────────────────────
    // blend_layer was refactored to parallelize via rayon (chunked) while
    // blend_chunk holds the original scalar inner loop. These tests verify
    // that parallel output is byte-identical to scalar output for every
    // blend mode, and that edge cases are handled.

    fn make_test_pattern(n_pixels: usize) -> Vec<u8> {
        // Deterministic noise — different bytes per pixel so any incorrect
        // chunk boundary would show up as pixel mismatch.
        (0..n_pixels * 4)
            .map(|i| ((i.wrapping_mul(131) ^ (i >> 2)) & 0xFF) as u8)
            .collect()
    }

    fn all_blend_modes() -> &'static [BlendMode] {
        &[
            BlendMode::Normal, BlendMode::Add, BlendMode::Multiply, BlendMode::Screen,
            BlendMode::Overlay, BlendMode::SoftLight, BlendMode::HardLight,
            BlendMode::Difference, BlendMode::Exclusion, BlendMode::ColorDodge,
            BlendMode::ColorBurn, BlendMode::Darken, BlendMode::Lighten,
        ]
    }

    #[test]
    fn parallel_blend_matches_scalar_large_buffer() {
        // 8192 pixels = 2× MIN_CHUNK_PIXELS — forces parallel path
        let n = 8192;
        let src = make_test_pattern(n);
        let base = make_test_pattern(n);

        for &mode in all_blend_modes() {
            let mut parallel = base.clone();
            blend_layer(&mut parallel, &src, 0.5, mode);

            let mut scalar = base.clone();
            blend_chunk(&mut scalar, &src, 0.5, mode);

            assert_eq!(parallel, scalar, "parallel vs scalar mismatch for {mode:?}");
        }
    }

    #[test]
    fn parallel_blend_matches_scalar_non_multiple_chunk_size() {
        // 8193 pixels — doesn't divide evenly by typical chunk size
        let n = 8193;
        let src = make_test_pattern(n);
        let base = make_test_pattern(n);

        let mut parallel = base.clone();
        blend_layer(&mut parallel, &src, 0.75, BlendMode::Normal);

        let mut scalar = base.clone();
        blend_chunk(&mut scalar, &src, 0.75, BlendMode::Normal);

        assert_eq!(parallel, scalar);
    }

    #[test]
    fn blend_layer_size_mismatch_returns_immediately() {
        // Existing behavior preserved
        let mut dst = vec![0u8; 40];
        let src = vec![255u8; 20];
        blend_layer(&mut dst, &src, 1.0, BlendMode::Normal);
        // dst unchanged
        assert!(dst.iter().all(|&b| b == 0));
    }

    #[test]
    fn blend_layer_empty_slice_no_panic() {
        let mut dst: Vec<u8> = vec![];
        let src: Vec<u8> = vec![];
        blend_layer(&mut dst, &src, 1.0, BlendMode::Normal);
        assert!(dst.is_empty());
    }

    #[test]
    fn blend_layer_single_pixel_correct() {
        // Below MIN_CHUNK_PIXELS — takes the single-chunk fast path
        let mut dst = vec![0, 0, 0, 255]; // black
        let src = vec![255, 255, 255, 255]; // white
        blend_layer(&mut dst, &src, 1.0, BlendMode::Normal);
        // White over black at full opacity = white
        for ch in 0..3 {
            assert!((dst[ch] as i16 - 255).abs() <= 1, "channel {ch}: {}", dst[ch]);
        }
    }

    #[test]
    fn blend_layer_small_buffer_below_min_chunk() {
        // 100 pixels — well below MIN_CHUNK_PIXELS (4096). Single-chunk path.
        let n = 100;
        let src = make_test_pattern(n);
        let base = make_test_pattern(n);

        let mut via_layer = base.clone();
        blend_layer(&mut via_layer, &src, 0.5, BlendMode::Multiply);

        let mut via_chunk = base.clone();
        blend_chunk(&mut via_chunk, &src, 0.5, BlendMode::Multiply);

        assert_eq!(via_layer, via_chunk);
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
