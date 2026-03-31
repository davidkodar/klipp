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

fn scale_rgba(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    use image::{ImageBuffer, Rgba};
    let img = match ImageBuffer::<Rgba<u8>, _>::from_raw(src_w, src_h, src.to_vec()) {
        Some(img) => img,
        None => return vec![0u8; (dst_w * dst_h * 4) as usize],
    };
    let scaled = image::imageops::resize(&img, dst_w, dst_h, image::imageops::FilterType::Triangle);
    scaled.into_raw()
}
