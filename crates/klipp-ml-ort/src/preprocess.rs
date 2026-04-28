//! BiRefNet pre-processing.
//!
//! Klipp's pipeline gives us linear-light f32 RGBA at the source
//! resolution. BiRefNet was trained on standard sRGB-gamma 8-bit
//! inputs, so the pre-processing has three steps:
//!
//! 1. Bilinear resample to the model input size (1024×1024 by default).
//! 2. Re-apply the sRGB transfer curve so the values land in the
//!    same numeric distribution the model saw during training.
//! 3. Per-channel normalise with ImageNet mean / std.
//!
//! The result is laid out NCHW (1, 3, H, W) f32 — the layout
//! ONNX Runtime expects when binding the input tensor.
//!
//! All tests live in this module. The functions are pure CPU and
//! deterministic — perfect for property-style coverage without a
//! real model.

use klipp_ml::InputFrame;

/// BiRefNet's standard input resolution. The pre-process resizes to
/// this regardless of source resolution.
pub const BIREFNET_INPUT_SIZE: u32 = 1024;

/// ImageNet per-channel mean (RGB order). Pinned by the BiRefNet
/// reference implementation (huggingface.co/onnx-community/BiRefNet_lite-ONNX).
pub const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];

/// ImageNet per-channel std (RGB order).
pub const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];

/// Output of [`preprocess`]: NCHW f32 ready to bind as an ORT input
/// tensor. `(1, 3, BIREFNET_INPUT_SIZE, BIREFNET_INPUT_SIZE)` —
/// 12,582,912 floats / 48 MB at 1024².
#[derive(Debug, Clone)]
pub struct PreprocessedTensor {
    /// NCHW f32 buffer. Shape is `(1, 3, height, width)` and length
    /// `3 * height * width`.
    pub data: Vec<f32>,
    /// Same as [`BIREFNET_INPUT_SIZE`] but kept on the struct so the
    /// post-process layer doesn't need to import the constant.
    pub width: u32,
    pub height: u32,
}

/// Run the full BiRefNet pre-process pipeline on a linear-light
/// RGBA frame.
pub fn preprocess(frame: &InputFrame<'_>) -> PreprocessedTensor {
    preprocess_to_size(frame, BIREFNET_INPUT_SIZE, BIREFNET_INPUT_SIZE)
}

/// Variant that accepts a custom output resolution. BiRefNet
/// always uses 1024², but exposing the size keeps the helper
/// reusable for future ort-backed models with different input
/// shapes (e.g. SAM2's 1024² image encoder, RF-DETR-Seg's 432²).
pub fn preprocess_to_size(
    frame: &InputFrame<'_>,
    out_w: u32,
    out_h: u32,
) -> PreprocessedTensor {
    let resized = bilinear_resize_rgba(
        frame.linear_rgba,
        frame.width,
        frame.height,
        out_w,
        out_h,
    );

    let pixel_count = (out_w as usize) * (out_h as usize);
    // NCHW: three contiguous planes, each pixel_count long.
    let mut data = vec![0.0_f32; 3 * pixel_count];
    let (r_plane, gb) = data.split_at_mut(pixel_count);
    let (g_plane, b_plane) = gb.split_at_mut(pixel_count);

    for i in 0..pixel_count {
        let lin_r = resized[i * 4];
        let lin_g = resized[i * 4 + 1];
        let lin_b = resized[i * 4 + 2];

        // BiRefNet's reference pre-process eats sRGB-gamma 0..1
        // values, then normalises. Klipp's pipeline gives us
        // linear-light, so encode back to sRGB before normalising.
        let s_r = klipp_color::transfer::linear_to_srgb(lin_r.clamp(0.0, 1.0));
        let s_g = klipp_color::transfer::linear_to_srgb(lin_g.clamp(0.0, 1.0));
        let s_b = klipp_color::transfer::linear_to_srgb(lin_b.clamp(0.0, 1.0));

        r_plane[i] = (s_r - IMAGENET_MEAN[0]) / IMAGENET_STD[0];
        g_plane[i] = (s_g - IMAGENET_MEAN[1]) / IMAGENET_STD[1];
        b_plane[i] = (s_b - IMAGENET_MEAN[2]) / IMAGENET_STD[2];
    }

    PreprocessedTensor {
        data,
        width: out_w,
        height: out_h,
    }
}

/// Hand-rolled bilinear resampler. Operates on interleaved RGBA
/// f32 data, returns an interleaved RGBA f32 buffer at the target
/// size. Kept hand-written to avoid pulling in the full `image`
/// crate just for resize — the implementation is ~40 lines and
/// pure float math.
///
/// Out-of-bound source coordinates clamp to the edge (CLAMP_TO_EDGE
/// addressing). Empty input or zero target size returns an empty
/// buffer; the caller is expected to short-circuit those cases.
pub fn bilinear_resize_rgba(
    src: &[f32],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Vec<f32> {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Vec::new();
    }
    let expected = (src_w as usize) * (src_h as usize) * 4;
    if src.len() < expected {
        return Vec::new();
    }

    let mut out = vec![0.0_f32; (dst_w as usize) * (dst_h as usize) * 4];

    let scale_x = (src_w as f32) / (dst_w as f32);
    let scale_y = (src_h as f32) / (dst_h as f32);

    for dy in 0..dst_h {
        // Centre-of-pixel sampling: shift by 0.5 so dy=0 samples the
        // top-left source pixel at (0.5, 0.5) rather than (0, 0). Same
        // convention as OpenCV INTER_LINEAR + the BiRefNet reference.
        let sy = ((dy as f32) + 0.5) * scale_y - 0.5;
        let sy0 = sy.floor().max(0.0).min((src_h - 1) as f32) as u32;
        let sy1 = (sy0 + 1).min(src_h - 1);
        let fy = (sy - sy0 as f32).clamp(0.0, 1.0);

        for dx in 0..dst_w {
            let sx = ((dx as f32) + 0.5) * scale_x - 0.5;
            let sx0 = sx.floor().max(0.0).min((src_w - 1) as f32) as u32;
            let sx1 = (sx0 + 1).min(src_w - 1);
            let fx = (sx - sx0 as f32).clamp(0.0, 1.0);

            let idx00 = ((sy0 * src_w + sx0) * 4) as usize;
            let idx01 = ((sy0 * src_w + sx1) * 4) as usize;
            let idx10 = ((sy1 * src_w + sx0) * 4) as usize;
            let idx11 = ((sy1 * src_w + sx1) * 4) as usize;

            let dst_idx = ((dy * dst_w + dx) * 4) as usize;
            for c in 0..4 {
                let p00 = src[idx00 + c];
                let p01 = src[idx01 + c];
                let p10 = src[idx10 + c];
                let p11 = src[idx11 + c];
                let top = p00 * (1.0 - fx) + p01 * fx;
                let bot = p10 * (1.0 - fx) + p11 * fx;
                out[dst_idx + c] = top * (1.0 - fy) + bot * fy;
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(w: u32, h: u32) -> Vec<f32> {
        // 2-colour checkerboard so resampling has something to chew on.
        let mut out = vec![0.0_f32; (w as usize) * (h as usize) * 4];
        for y in 0..h {
            for x in 0..w {
                let v = if (x + y) % 2 == 0 { 1.0 } else { 0.0 };
                let i = ((y * w + x) * 4) as usize;
                out[i] = v;
                out[i + 1] = v;
                out[i + 2] = v;
                out[i + 3] = 1.0;
            }
        }
        out
    }

    #[test]
    fn resize_identity_returns_input_pixels() {
        let src = checker(4, 4);
        let out = bilinear_resize_rgba(&src, 4, 4, 4, 4);
        assert_eq!(out.len(), src.len());
        for (a, b) in src.iter().zip(out.iter()) {
            assert!((a - b).abs() < 1e-4, "identity resize must preserve pixels");
        }
    }

    #[test]
    fn resize_handles_empty_input() {
        let out = bilinear_resize_rgba(&[], 0, 0, 16, 16);
        assert!(out.is_empty());
    }

    #[test]
    fn resize_handles_zero_target() {
        let src = checker(4, 4);
        let out = bilinear_resize_rgba(&src, 4, 4, 0, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn resize_short_input_returns_empty() {
        // Caller passed wrong shape — defensive return rather than
        // an out-of-bounds panic.
        let out = bilinear_resize_rgba(&[0.0; 8], 4, 4, 8, 8);
        assert!(out.is_empty());
    }

    #[test]
    fn resize_upsample_preserves_solid_colour() {
        // A solid red frame upsampled stays solid red — sanity that
        // bilinear weights sum to 1 at every output pixel.
        let mut src = vec![0.0_f32; 4 * 4 * 4];
        for px in src.chunks_exact_mut(4) {
            px[0] = 0.7;
            px[1] = 0.2;
            px[2] = 0.1;
            px[3] = 1.0;
        }
        let out = bilinear_resize_rgba(&src, 4, 4, 16, 16);
        for px in out.chunks_exact(4) {
            assert!((px[0] - 0.7).abs() < 1e-4);
            assert!((px[1] - 0.2).abs() < 1e-4);
            assert!((px[2] - 0.1).abs() < 1e-4);
            assert!((px[3] - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn resize_downsample_averages_block() {
        // 2×2 block of distinct values downsamples to a single
        // averaged pixel. Pinning the average so subtle changes to
        // the resampler get caught.
        let src = vec![
            // Row 0: red 1.0, green 0.0
            1.0, 0.0, 0.0, 1.0,
            0.0, 1.0, 0.0, 1.0,
            // Row 1: blue 0.0, white 1.0
            0.0, 0.0, 1.0, 1.0,
            1.0, 1.0, 1.0, 1.0,
        ];
        let out = bilinear_resize_rgba(&src, 2, 2, 1, 1);
        assert_eq!(out.len(), 4);
        // The single output pixel should be the mean of the four.
        // Mean R = (1+0+0+1)/4 = 0.5, G = (0+1+0+1)/4 = 0.5, B = (0+0+1+1)/4 = 0.5, A = 1
        assert!((out[0] - 0.5).abs() < 0.05, "R was {}", out[0]);
        assert!((out[1] - 0.5).abs() < 0.05, "G was {}", out[1]);
        assert!((out[2] - 0.5).abs() < 0.05, "B was {}", out[2]);
        assert!((out[3] - 1.0).abs() < 0.05, "A was {}", out[3]);
    }

    #[test]
    fn preprocess_output_shape_matches_birefnet() {
        let src = checker(8, 8);
        let frame = InputFrame {
            width: 8,
            height: 8,
            linear_rgba: &src,
        };
        let out = preprocess(&frame);
        assert_eq!(out.width, BIREFNET_INPUT_SIZE);
        assert_eq!(out.height, BIREFNET_INPUT_SIZE);
        let expected_len = 3 * (BIREFNET_INPUT_SIZE as usize) * (BIREFNET_INPUT_SIZE as usize);
        assert_eq!(out.data.len(), expected_len);
    }

    #[test]
    fn preprocess_normalise_solid_grey_matches_imagenet_stats() {
        // Solid mid-grey (linear 0.5 ≈ sRGB 0.735). Each channel
        // after sRGB encode + ImageNet normalize lands at
        // (0.735 - mean) / std. Verifying lets us catch a swap of
        // mean/std or a missed sRGB encode.
        let pixels: Vec<f32> = (0..16 * 16)
            .flat_map(|_| [0.5_f32, 0.5, 0.5, 1.0])
            .collect();
        let frame = InputFrame {
            width: 16,
            height: 16,
            linear_rgba: &pixels,
        };
        let out = preprocess_to_size(&frame, 8, 8);
        let pixel_count = 8 * 8;
        let s_grey = klipp_color::transfer::linear_to_srgb(0.5);
        let exp_r = (s_grey - IMAGENET_MEAN[0]) / IMAGENET_STD[0];
        let exp_g = (s_grey - IMAGENET_MEAN[1]) / IMAGENET_STD[1];
        let exp_b = (s_grey - IMAGENET_MEAN[2]) / IMAGENET_STD[2];

        for i in 0..pixel_count {
            assert!((out.data[i] - exp_r).abs() < 1e-3, "R channel pixel {i}");
            assert!(
                (out.data[pixel_count + i] - exp_g).abs() < 1e-3,
                "G channel pixel {i}"
            );
            assert!(
                (out.data[2 * pixel_count + i] - exp_b).abs() < 1e-3,
                "B channel pixel {i}"
            );
        }
    }

    #[test]
    fn preprocess_layout_is_nchw_planar() {
        // R / G / B go in three contiguous planes, not interleaved.
        // Drop a single bright-red pixel into a black field; only
        // the R plane should have a non-trivial value.
        let mut pixels = vec![0.0_f32; 4 * 4 * 4];
        pixels[0] = 1.0; // R channel at (0,0)
        pixels[3] = 1.0; // alpha
        let frame = InputFrame {
            width: 4,
            height: 4,
            linear_rgba: &pixels,
        };
        let out = preprocess_to_size(&frame, 4, 4);
        let pixel_count = 4 * 4;
        // After ImageNet normalize, "all black" pixels still produce
        // a uniform negative offset (= -mean/std). The single bright
        // red pixel moves the R-plane sample upward; we just need to
        // confirm the R plane's first sample differs from G/B planes'
        // first samples (the layout is correct iff the planes diverge).
        let r0 = out.data[0];
        let g0 = out.data[pixel_count];
        let b0 = out.data[2 * pixel_count];
        assert!(
            r0 > g0 + 0.5,
            "R plane should have higher value than G plane after a bright red pixel: r={r0} g={g0}"
        );
        assert!(
            r0 > b0 + 0.5,
            "R plane should have higher value than B plane: r={r0} b={b0}"
        );
    }
}
