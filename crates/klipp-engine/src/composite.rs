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
            // Scale inline — only allocate when needed
            &scale_rgba(&layer.rgba, layer.width, layer.height, out_width, out_height)
        };
        blend_over(&mut output, src, layer.opacity);
    }
    output
}

fn blend_over(dst: &mut [u8], src: &[u8], opacity: f32) {
    if src.len() != dst.len() {
        return;
    }
    for i in (0..dst.len()).step_by(4) {
        let sa = (src[i + 3] as f32 / 255.0) * opacity;
        let da = dst[i + 3] as f32 / 255.0;
        let out_a = sa + da * (1.0 - sa);
        if out_a > 0.0 {
            dst[i] = ((src[i] as f32 * sa + dst[i] as f32 * da * (1.0 - sa)) / out_a) as u8;
            dst[i + 1] = ((src[i + 1] as f32 * sa + dst[i + 1] as f32 * da * (1.0 - sa)) / out_a) as u8;
            dst[i + 2] = ((src[i + 2] as f32 * sa + dst[i + 2] as f32 * da * (1.0 - sa)) / out_a) as u8;
            dst[i + 3] = (out_a * 255.0) as u8;
        }
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
