//! Mask preview overlay rendering for the AI Tools system.
//!
//! Paints the latest [`klipp_ml::MaskBuffer`] onto the program viewer
//! as a semi-transparent coloured fill, so the user can see what the
//! model proposed before they accept it. The colour is parameterised
//! (default elsewhere is orange-ish at 50% alpha) so future "compare"
//! UIs can stack two masks in different colours.
//!
//! ## v1 implementation
//!
//! Per-pixel blit through an `egui::ColorImage`:
//!
//! 1. Walk the mask buffer once, fill an `egui::ColorImage` whose
//!    pixel `(x, y)` is `Color32::from_rgba_premultiplied(r, g, b, a)`
//!    — the mask alpha scaled by the chosen overlay colour's alpha.
//! 2. Upload as an egui texture.
//! 3. Paint with `Painter::image` over the viewer's image rect.
//!
//! Performance optimisation is deferred: mask updates are
//! user-initiated (one click → one mask), not per-frame, so we don't
//! pay the upload cost more than once per user action. When SAM2
//! video propagation lands, the mask will refresh per frame and this
//! module will need a GPU path (sampler + premultiply shader). The
//! API stays the same — only the body of [`render_mask_overlay`]
//! changes.
//!
//! ## Coordinate spaces
//!
//! `rect` is the viewer's image rect in screen-space pixels (the
//! same `Rect` the viewer's `painter.image` call uses for the actual
//! frame). The mask is in image space; egui's `image()` call handles
//! the resample on draw, which is exactly what we want — mask + frame
//! line up because they share the unit-square UV layout.

use klipp_ml::{MaskBuffer, MaskCandidate};

/// Distinct hues for multi-detection rendering. Cycled by
/// candidate index so RF-DETR-style multi-object outputs paint with
/// different colors per detection rather than blurring into one
/// orange blob. Picked for high contrast between adjacent palette
/// entries — helps the eye separate overlapping detections.
pub const DETECTION_PALETTE: &[egui::Color32] = &[
    egui::Color32::from_rgba_premultiplied(255, 140, 0, 128), // orange (default)
    egui::Color32::from_rgba_premultiplied(80, 200, 240, 128), // cyan
    egui::Color32::from_rgba_premultiplied(220, 80, 220, 128), // magenta
    egui::Color32::from_rgba_premultiplied(80, 220, 100, 128), // green
    egui::Color32::from_rgba_premultiplied(255, 220, 60, 128), // yellow
    egui::Color32::from_rgba_premultiplied(220, 80, 80, 128),  // red
    egui::Color32::from_rgba_premultiplied(120, 100, 240, 128), // indigo
    egui::Color32::from_rgba_premultiplied(255, 180, 200, 128), // pink
];

/// Pick the palette color for a detection by index. Wraps modulo
/// the palette size so indices beyond the palette length still
/// produce a sensible color.
pub fn detection_color(index: usize) -> egui::Color32 {
    DETECTION_PALETTE[index % DETECTION_PALETTE.len()]
}

/// Build a `TextureHandle` for the supplied mask. Caller owns the
/// returned handle and is responsible for dropping the previous one
/// through `KlippApp::defer_drop_texture` — dropping a `TextureHandle`
/// mid-`update()` panics wgpu on the next submit
/// (see `feedback_defer_drop_texture`).
///
/// Returns `None` for empty / malformed masks so the caller can skip
/// painting without holding a stale handle.
pub fn build_mask_texture(
    ctx: &egui::Context,
    mask: &MaskBuffer,
    color: egui::Color32,
    texture_label: &str,
) -> Option<egui::TextureHandle> {
    if mask.width == 0 || mask.height == 0 {
        return None;
    }
    let expected_len = (mask.width as usize) * (mask.height as usize);
    if mask.alpha.len() != expected_len {
        return None;
    }
    let image = colorize_mask(mask, color);
    Some(ctx.load_texture(texture_label, image, egui::TextureOptions::LINEAR))
}

/// Paint an already-built overlay texture into `rect`. Pure draw
/// command — no allocation, no upload, no handle drop.
pub fn paint_mask_texture(
    painter: &egui::Painter,
    rect: egui::Rect,
    texture: &egui::TextureHandle,
) {
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    painter.image(texture.id(), rect, uv, egui::Color32::WHITE);
}

/// Render a mask as a semi-transparent coloured overlay on the
/// viewer image. **Test/standalone path only** — performs an
/// upload-then-drop in one call, which is fine outside an
/// `eframe::App::update` body but unsafe inside it (see
/// `build_mask_texture` + `paint_mask_texture` for the production
/// split). Safe with empty masks — they short-circuit.
pub fn render_mask_overlay(
    painter: &egui::Painter,
    rect: egui::Rect,
    mask: &MaskBuffer,
    color: egui::Color32,
) {
    render_mask_overlay_with_label(painter, rect, mask, color, "ai_tools_mask_overlay");
}

/// Variant of [`render_mask_overlay`] that exposes the texture label.
pub fn render_mask_overlay_with_label(
    painter: &egui::Painter,
    rect: egui::Rect,
    mask: &MaskBuffer,
    color: egui::Color32,
    texture_label: &str,
) {
    let Some(texture) = build_mask_texture(painter.ctx(), mask, color, texture_label) else {
        return;
    };
    paint_mask_texture(painter, rect, &texture);
}

/// Build a `TextureHandle` for the multi-detection overlay
/// (RF-DETR-Seg's many candidates). Each pixel takes the color of
/// the candidate with the highest alpha at that location, modulated
/// by that alpha. Per-candidate colors come from
/// [`DETECTION_PALETTE`].
///
/// Falls back to a single-color overlay when only one candidate
/// is present (matches the previous behaviour for BiRefNet /
/// MobileSAM outputs). Returns `None` for an empty `candidates`
/// slice or shape-mismatched masks.
pub fn build_multi_detection_texture(
    ctx: &egui::Context,
    candidates: &[MaskCandidate],
    fallback_color: egui::Color32,
    texture_label: &str,
) -> Option<egui::TextureHandle> {
    let first = candidates.first()?;
    if first.mask.width == 0 || first.mask.height == 0 {
        return None;
    }
    let w = first.mask.width as usize;
    let h = first.mask.height as usize;
    let pixel_count = w * h;
    if first.mask.alpha.len() != pixel_count {
        return None;
    }

    let image = if candidates.len() == 1 {
        // Single-detection path — same look as the legacy single-
        // candidate overlay, just routed through this entry point.
        colorize_mask(&first.mask, fallback_color)
    } else {
        composite_multi_detection(candidates, w, h)
    };
    Some(ctx.load_texture(texture_label, image, egui::TextureOptions::LINEAR))
}

/// CPU compositor: for each pixel, find the candidate with the
/// highest mask alpha at that pixel and tint with that candidate's
/// palette color. O(N × pixels) where N = number of candidates.
/// At 1080p × 64 candidates that's ~130 M cmps — fits in a frame
/// budget on any modern CPU.
fn composite_multi_detection(
    candidates: &[MaskCandidate],
    w: usize,
    h: usize,
) -> egui::ColorImage {
    let pixel_count = w * h;
    let mut pixels = vec![egui::Color32::TRANSPARENT; pixel_count];

    for (idx, cand) in candidates.iter().enumerate() {
        if cand.mask.width as usize != w
            || cand.mask.height as usize != h
            || cand.mask.alpha.len() != pixel_count
        {
            continue;
        }
        let color = detection_color(idx);
        let cr = color.r() as u32;
        let cg = color.g() as u32;
        let cb = color.b() as u32;
        let ca = color.a() as u32;

        for i in 0..pixel_count {
            let m = cand.mask.alpha[i] as u32;
            if m == 0 {
                continue;
            }
            let combined_a = (ca * m) / 255;
            // Keep the highest-alpha winner per pixel — this
            // separates overlapping detections visually rather than
            // blending them into mud.
            if (combined_a as u8) > pixels[i].a() {
                let r = (cr * combined_a) / 255;
                let g = (cg * combined_a) / 255;
                let b = (cb * combined_a) / 255;
                pixels[i] = egui::Color32::from_rgba_premultiplied(
                    r as u8, g as u8, b as u8, combined_a as u8,
                );
            }
        }
    }

    egui::ColorImage { size: [w, h], pixels }
}

/// Build the `ColorImage` that backs the overlay texture. Public for
/// testing — the upload itself needs an egui context, but the
/// per-pixel blend is pure CPU and we want to verify it.
pub fn colorize_mask(mask: &MaskBuffer, color: egui::Color32) -> egui::ColorImage {
    let pixel_count = (mask.width as usize) * (mask.height as usize);
    let mut pixels = Vec::with_capacity(pixel_count);

    // egui's `Color32` is **premultiplied** alpha. To preserve the
    // colour at every alpha value, we multiply each RGB channel by
    // (mask_alpha / 255) * (color_alpha / 255). Doing it in one
    // integer step avoids a float round-trip per pixel.
    let cr = color.r() as u32;
    let cg = color.g() as u32;
    let cb = color.b() as u32;
    let ca = color.a() as u32;

    for &m in &mask.alpha {
        // Combined alpha [0..255]
        let a = (ca * m as u32) / 255;
        let r = (cr * a) / 255;
        let g = (cg * a) / 255;
        let b = (cb * a) / 255;
        pixels.push(egui::Color32::from_rgba_premultiplied(
            r as u8, g as u8, b as u8, a as u8,
        ));
    }

    egui::ColorImage {
        size: [mask.width as usize, mask.height as usize],
        pixels,
    }
}

/// Suggested default overlay colour: orange at ~50% alpha. Matches
/// the colour Klipp's existing Track Selection ghost uses, so the
/// mask preview reads as "in the family of selection chrome".
pub const DEFAULT_MASK_OVERLAY_COLOR: egui::Color32 =
    egui::Color32::from_rgba_premultiplied(255, 140, 0, 128);

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> egui::Context {
        egui::Context::default()
    }

    fn painter_for(ctx: &egui::Context, rect: egui::Rect) -> egui::Painter {
        // Painter requires a real LayerId + ClipRect. We use a
        // background layer because background painters don't require
        // an active Window/Area scope.
        let layer = egui::LayerId::background();
        ctx.layer_painter(layer).with_clip_rect(rect)
    }

    #[test]
    fn empty_mask_render_is_safe() {
        let ctx = ctx();
        let _ = ctx.run(Default::default(), |ctx| {
            let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(64.0, 64.0));
            let painter = painter_for(ctx, rect);
            let mask = MaskBuffer::empty(0, 0);
            render_mask_overlay(&painter, rect, &mask, DEFAULT_MASK_OVERLAY_COLOR);
        });
    }

    #[test]
    fn zero_height_mask_render_is_safe() {
        let ctx = ctx();
        let _ = ctx.run(Default::default(), |ctx| {
            let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(8.0, 8.0));
            let painter = painter_for(ctx, rect);
            let mask = MaskBuffer {
                width: 4,
                height: 0,
                alpha: vec![],
            };
            render_mask_overlay(&painter, rect, &mask, DEFAULT_MASK_OVERLAY_COLOR);
        });
    }

    #[test]
    fn malformed_mask_alpha_length_does_not_panic() {
        // Buffer with width * height != alpha.len(). Should be
        // caught by the defensive check inside render_mask_overlay.
        let ctx = ctx();
        let _ = ctx.run(Default::default(), |ctx| {
            let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(8.0, 8.0));
            let painter = painter_for(ctx, rect);
            let mask = MaskBuffer {
                width: 4,
                height: 4,
                alpha: vec![255; 7], // intentionally wrong
            };
            render_mask_overlay(&painter, rect, &mask, DEFAULT_MASK_OVERLAY_COLOR);
        });
    }

    #[test]
    fn render_with_real_mask_does_not_panic() {
        let ctx = ctx();
        let _ = ctx.run(Default::default(), |ctx| {
            let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(80.0, 60.0));
            let painter = painter_for(ctx, rect);
            // 16x12 buffer, half-filled.
            let mut alpha = vec![0u8; 16 * 12];
            for y in 0..12 {
                for x in 0..8 {
                    alpha[y * 16 + x] = 255;
                }
            }
            let mask = MaskBuffer {
                width: 16,
                height: 12,
                alpha,
            };
            render_mask_overlay(&painter, rect, &mask, DEFAULT_MASK_OVERLAY_COLOR);
        });
    }

    #[test]
    fn colorize_zero_alpha_pixels_are_transparent() {
        let mask = MaskBuffer::empty(2, 2);
        let img = colorize_mask(&mask, DEFAULT_MASK_OVERLAY_COLOR);
        assert_eq!(img.size, [2, 2]);
        for px in &img.pixels {
            assert_eq!(px.a(), 0);
            assert_eq!(px.r(), 0);
            assert_eq!(px.g(), 0);
            assert_eq!(px.b(), 0);
        }
    }

    #[test]
    fn colorize_full_alpha_matches_overlay_color() {
        // mask alpha 255 + overlay color alpha 255 → output equals
        // the overlay colour (premultiplied so RGB = colour RGB).
        let mask = MaskBuffer::solid(2, 2, 255);
        let color = egui::Color32::from_rgba_premultiplied(200, 100, 50, 255);
        let img = colorize_mask(&mask, color);
        for px in &img.pixels {
            assert_eq!(px.r(), 200);
            assert_eq!(px.g(), 100);
            assert_eq!(px.b(), 50);
            assert_eq!(px.a(), 255);
        }
    }

    #[test]
    fn colorize_partial_mask_alpha_modulates_output() {
        // mask alpha 128 + overlay color alpha 255 → output alpha ~128
        // and RGB scaled accordingly.
        let mask = MaskBuffer::solid(1, 1, 128);
        let color = egui::Color32::from_rgba_premultiplied(255, 255, 255, 255);
        let img = colorize_mask(&mask, color);
        let px = img.pixels[0];
        // Allow off-by-one for integer division.
        assert!(px.a() >= 127 && px.a() <= 129, "alpha {}", px.a());
        assert!(px.r() >= 127 && px.r() <= 129);
        assert!(px.g() >= 127 && px.g() <= 129);
        assert!(px.b() >= 127 && px.b() <= 129);
    }

    #[test]
    fn colorize_partial_overlay_alpha_modulates_output() {
        // mask alpha 255 + overlay color alpha 64 → output alpha ~64.
        let mask = MaskBuffer::solid(1, 1, 255);
        let color = egui::Color32::from_rgba_premultiplied(255, 0, 0, 64);
        let img = colorize_mask(&mask, color);
        let px = img.pixels[0];
        assert!(px.a() >= 63 && px.a() <= 65, "alpha {}", px.a());
        // R was 255 in colour, so R after premultiply ≈ alpha.
        assert!(px.r() >= 63 && px.r() <= 65);
        assert_eq!(px.g(), 0);
        assert_eq!(px.b(), 0);
    }

    #[test]
    fn default_overlay_color_is_orange_ish_at_half_alpha() {
        let c = DEFAULT_MASK_OVERLAY_COLOR;
        // The constant is in the orange family with alpha around 0.5.
        assert!(c.a() > 0 && c.a() < 200);
        // Red dominant.
        assert!(c.r() > c.g());
        assert!(c.r() > c.b());
    }
}
