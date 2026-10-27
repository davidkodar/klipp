//! The GPU layer stack is the CPU stack: every blend mode at several
//! opacities lands within a level of `blend_layer_linear`, a sprite
//! decodes as `srgb_u8_to_linear_f32_rgba` does, and the first Normal
//! layer moves in untouched.

use klipp_effects::BlendMode;
use klipp_effects::composite::blend_layer_linear;
use klipp_effects::gpu::GpuEffectsPipeline;

const W: u32 = 64;
const H: u32 = 48;

fn pipeline() -> Option<GpuEffectsPipeline> {
    let p = GpuEffectsPipeline::new();
    if p.is_none() {
        eprintln!("skipping: no usable GPU");
    }
    p
}

/// A frame with gradients in every channel, alpha included, and some
/// values above one so the HDR-safe modes are exercised.
fn frame(seed: f32) -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let fx = x as f32 / (W - 1) as f32;
            let fy = y as f32 / (H - 1) as f32;
            v.extend_from_slice(&[
                (fx * 1.3 + seed * 0.1).fract() * 1.2,
                (fy + seed * 0.3).fract(),
                ((fx + fy) * 0.5 + seed * 0.2).fract(),
                if seed > 0.5 { (fx * 3.0).fract() } else { 1.0 },
            ]);
        }
    }
    v
}

fn all_modes() -> [BlendMode; 13] {
    [
        BlendMode::Normal, BlendMode::Add, BlendMode::Multiply, BlendMode::Screen, BlendMode::Overlay,
        BlendMode::SoftLight, BlendMode::HardLight, BlendMode::Difference, BlendMode::Exclusion,
        BlendMode::ColorDodge, BlendMode::ColorBurn, BlendMode::Darken, BlendMode::Lighten,
    ]
}

/// A frame of bytes as the title renderer hands a sprite on: the box it
/// covers in half floats (#190).
fn raster(rgba: &[u8]) -> klipp_title::BoxRaster {
    klipp_title::BoxRaster::from_bytes(rgba, (W, H), (0, H), (0, W))
}

fn sprite_box(r: &klipp_title::BoxRaster) -> klipp_effects::gpu::SpriteBox {
    klipp_effects::gpu::SpriteBox { x0: r.x0, y0: r.y0, w: r.w, h: r.h }
}

/// A CPU twin's frame (byte units, unrounded) as a layer, through the
/// half-float box upload.
fn upload_stack(gpu: &mut klipp_effects::gpu::GpuEffectsPipeline, stack: &[f32]) -> bool {
    let r = klipp_title::BoxRaster::from_stack(stack, (W, H), (0, H), (0, W));
    gpu.stack_layer_from_srgb_f16_box(0, &r.texels, sprite_box(&r))
}

fn worst_level(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max) * 255.0
}

#[test]
fn every_blend_mode_matches_the_cpu_stack() {
    let Some(mut gpu) = pipeline() else { return };
    let base = frame(0.2);
    let top = frame(0.8);
    for mode in all_modes() {
        for opacity in [1.0f32, 0.5, 0.25] {
            let mut cpu = base.clone();
            blend_layer_linear(&mut cpu, &top, opacity, mode);

            assert!(gpu.stack_begin(W, H, 2));
            assert!(gpu.stack_layer_from_linear(0, &base));
            assert!(gpu.stack_layer_from_linear(1, &top));
            assert!(gpu.stack_blend(0, 1.0, 0));
            assert!(gpu.stack_blend(1, opacity, mode as u32));
            assert!(gpu.stack_finish_into_chain(true));
            let got = gpu.stack_download_linear();
            let worst = worst_level(&cpu, &got);
            assert!(worst <= 1.0, "{mode:?} at {opacity}: GPU differs from CPU by {worst:.2} levels");
        }
    }
}

#[test]
fn a_sprite_decodes_as_the_cpu_bridge_does() {
    let Some(mut gpu) = pipeline() else { return };
    let mut rgba = Vec::with_capacity((W * H * 4) as usize);
    for i in 0..(W * H) {
        rgba.extend_from_slice(&[(i % 251) as u8, (i * 7 % 256) as u8, (i * 13 % 256) as u8, (i * 3 % 256) as u8]);
    }
    let cpu = klipp_effects::srgb_u8_to_linear_f32_rgba(&rgba);
    assert!(gpu.stack_begin(W, H, 1));
    assert!(gpu.stack_layer_from_srgb_u8(0, &rgba));
    assert!(gpu.stack_blend(0, 1.0, 0));
    assert!(gpu.stack_finish_into_chain(true));
    let got = gpu.stack_download_linear();
    let worst = worst_level(&cpu, &got);
    assert!(worst <= 1.0, "sprite decode differs by {worst:.2} levels");
    // Straight alpha survives the bridge.
    assert!((got[3] - rgba[3] as f32 / 255.0).abs() < 1e-3);
}

#[test]
fn the_finish_premultiplies_like_the_cpu_stack() {
    let Some(mut gpu) = pipeline() else { return };
    let layer = frame(0.9);
    let mut cpu = layer.clone();
    for px in cpu.chunks_exact_mut(4) {
        let a = px[3].clamp(0.0, 1.0);
        px[0] *= a;
        px[1] *= a;
        px[2] *= a;
        px[3] = 1.0;
    }
    assert!(gpu.stack_begin(W, H, 1));
    assert!(gpu.stack_layer_from_linear(0, &layer));
    assert!(gpu.stack_blend(0, 1.0, 0));
    assert!(gpu.stack_finish_into_chain(false));
    let got = gpu.stack_download_linear();
    let worst = worst_level(&cpu, &got);
    assert!(worst <= 1.0, "premultiply differs by {worst:.2} levels");
}

/// The viewer's channel view (#2758) finishes the stack as its CPU twin
/// does, in every channel: the picture, red, green, blue, the alpha
/// before the flatten, and luma by an ACEScg row (not Rec.709's, so the
/// row is seen to be the one passed).
#[test]
fn the_channel_view_finishes_like_the_cpu_stack() {
    let Some(mut gpu) = pipeline() else { return };
    let layer = frame(0.9);
    let luma = [0.272_228_72, 0.674_081_77, 0.053_689_52];
    for channel in 0..=5u32 {
        let mut cpu = layer.clone();
        klipp_effects::composite::view_channel(&mut cpu, channel, luma);
        assert!(gpu.stack_begin(W, H, 1));
        assert!(gpu.stack_layer_from_linear(0, &layer));
        assert!(gpu.stack_blend(0, 1.0, 0));
        assert!(gpu.stack_finish_viewing(channel, luma));
        let got = gpu.stack_download_linear();
        let worst = worst_level(&cpu, &got);
        assert!(worst <= 1.0, "channel {channel} differs by {worst:.2} levels");
        if channel == 4 {
            // Alpha reads the coverage, in grey, flattened opaque.
            assert!(got.chunks_exact(4).zip(layer.chunks_exact(4)).all(|(g, l)| (g[0] - l[3]).abs() < 2e-3 && g[3] == 1.0));
        }
    }
}

/// A resident sprite placed by the GPU is the sprite shifted and faded
/// on the CPU and decoded as rows: the same layer, and no second upload
/// for the second placement.
#[test]
fn a_placed_sprite_is_the_shifted_and_faded_upload() {
    let Some(mut gpu) = pipeline() else { return };
    let mut rgba = Vec::with_capacity((W * H * 4) as usize);
    for i in 0..(W * H) {
        rgba.extend_from_slice(&[(i % 251) as u8, (i * 7 % 256) as u8, (i * 13 % 256) as u8, (i * 3 % 256) as u8]);
    }
    let (w, h) = (W as i32, H as i32);
    for (dx, dy, opacity) in [(0, 0, 1.0f32), (5, -3, 1.0), (-7, 4, 0.5), (0, 9, 0.37)] {
        // The CPU twin: `klipp_title`'s `shift_and_fade`.
        let mut shifted = vec![0u8; rgba.len()];
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = (x - dx, y - dy);
                if sx < 0 || sy < 0 || sx >= w || sy >= h {
                    continue;
                }
                let d = ((y * w + x) * 4) as usize;
                let so = ((sy * w + sx) * 4) as usize;
                shifted[d..d + 4].copy_from_slice(&rgba[so..so + 4]);
                if opacity < 0.9999 {
                    shifted[d + 3] = ((shifted[d + 3] as f32) * opacity).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        let y0 = dy.max(0) as u32;
        let y1 = (h + dy.min(0)).max(0) as u32;
        assert!(gpu.stack_begin(W, H, 1));
        assert!(gpu.stack_layer_from_srgb_u8_rows(0, &shifted, y0, y1));
        assert!(gpu.stack_blend(0, 1.0, 0));
        assert!(gpu.stack_finish_into_chain(true));
        let want = gpu.stack_download_linear();

        assert!(gpu.stack_begin(W, H, 1));
        let r = raster(&rgba);
        assert!(gpu.stack_layer_place_sprite(0, 77, &r.texels, sprite_box(&r), dx, dy, opacity, y0, y1));
        assert!(gpu.stack_sprite_resident(77));
        assert!(gpu.stack_blend(0, 1.0, 0));
        assert!(gpu.stack_finish_into_chain(true));
        let got = gpu.stack_download_linear();
        // The sprite's bytes in half floats, and its alpha faded unrounded
        // where the reference above rounds it to a whole level: half a
        // level from that rounding, a little more from the half floats.
        let worst = worst_level(&want, &got);
        let at = want.iter().zip(&got).enumerate().max_by(|a, b| (a.1 .0 - a.1 .1).abs().total_cmp(&(b.1 .0 - b.1 .1).abs())).map(|(i, _)| i).unwrap_or(0);
        let px = at / 4;
        assert!(worst <= 0.6, "placed sprite ({dx}, {dy}, {opacity}) differs from the shifted upload by {worst:.3} levels at {px}: want {:?} got {:?} bytes {:?}", &want[px * 4..px * 4 + 4], &got[px * 4..px * 4 + 4], &shifted[px * 4..px * 4 + 4]);
    }
}

/// A sprite posed — scaled about its anchor, faded, laid over a
/// full-frame colour, clipped to its rows — is `klipp_title::pose_sprite`
/// decoded as rows, to a level: the same words an entrance moves, on the
/// GPU where the compositor keeps them.
#[test]
fn a_posed_sprite_is_the_cpu_pose_uploaded() {
    let Some(mut gpu) = pipeline() else { return };
    // A soft blob with a hard edge, so scaling and blending both matter.
    let mut rgba = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let d = (((x as f32 - 20.0).powi(2) + (y as f32 - 12.0).powi(2)).sqrt() / 9.0).min(1.0);
            let a = ((1.0 - d) * 255.0) as u8;
            let i = ((y * W + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[(x * 9 % 256) as u8, 200, (y * 17 % 256) as u8, if x > 30 { 255 } else { a }]);
        }
    }
    let anchor = (20.0f32, 12.0f32);
    let cases: [(i32, i32, f32, f32, Option<[u8; 4]>, (u32, u32)); 6] = [
        (0, 0, 1.0, 1.0, None, (0, H)),
        (3, -2, 0.7, 1.3, None, (0, H)),
        (0, 4, 1.0, 0.6, None, (0, H)),
        (0, 0, 1.0, 1.0, Some([16, 20, 24, 255]), (0, H)),
        (-2, 1, 0.5, 1.15, Some([200, 40, 90, 255]), (0, H)),
        (0, 6, 1.0, 1.0, None, (8, 16)),
    ];
    for (dx, dy, opacity, scale, under, rows) in cases {
        let r = raster(&rgba);
        let want_stack = klipp_title::pose_sprite(&r, W, H, dx, dy, opacity, scale, anchor, under, None, rows);
        assert!(gpu.stack_begin(W, H, 1));
        assert!(upload_stack(&mut gpu, &want_stack));
        assert!(gpu.stack_blend(0, 1.0, 0));
        assert!(gpu.stack_finish_into_chain(true));
        let want = gpu.stack_download_linear();

        assert!(gpu.stack_begin(W, H, 1));
        assert!(gpu.stack_layer_place_sprite_posed(0, 91, &r.texels, sprite_box(&r), dx, dy, opacity, scale, anchor, under, None, rows.0, rows.1));
        assert!(gpu.stack_blend(0, 1.0, 0));
        assert!(gpu.stack_finish_into_chain(true));
        let got = gpu.stack_download_linear();
        let worst = worst_level(&want, &got);
        // Both unrounded (#190): what is left is the twin's frame going up
        // in half floats and the GPU's float against the CPU's.
        let tolerance = 0.5;
        let at = want.iter().zip(&got).enumerate().max_by(|a, b| (a.1 .0 - a.1 .1).abs().total_cmp(&(b.1 .0 - b.1 .1).abs())).map(|(i, _)| i).unwrap_or(0);
        let px = at / 4;
        assert!(
            worst <= tolerance,
            "posed sprite ({dx}, {dy}, {opacity}, ×{scale}, {under:?}, rows {rows:?}) differs by {worst:.3} levels at pixel ({}, {}) channel {}: twin {:?} gpu {:?}, twin stack {:?}",
            px % W as usize,
            px / W as usize,
            at % 4,
            &want[px * 4..px * 4 + 4],
            &got[px * 4..px * 4 + 4],
            &want_stack[px * 4..px * 4 + 4],
        );
    }
}

/// A layer's fade by where it is (#592) — edge fade zones at any of
/// the four edges, a roll's focus band down or across the frame, on a
/// sprite moved, scaled and laid over a colour — is
/// `klipp_title::pose_sprite` with the same `ScreenFade`, to a level:
/// the viewer fades as an export does.
#[test]
fn a_faded_sprite_is_the_cpu_fade_uploaded() {
    use klipp_core::title_motion::{EdgeFade, FocusBand, RampAxis, ScreenFade};
    let Some(mut gpu) = pipeline() else { return };
    // Solid across the frame, so the fade is all there is to see.
    let mut rgba = vec![0u8; (W * H * 4) as usize];
    for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
        px.copy_from_slice(&[(i * 7 % 256) as u8, 180, 60, 255]);
    }
    let roll = EdgeFade { top: 0.3, bottom: 0.3, ..EdgeFade::NONE };
    let lower_third = EdgeFade { left: 0.25, ..EdgeFade::NONE };
    let all = EdgeFade { top: 0.1, bottom: 0.2, left: 0.15, right: 0.05 };
    let band = |axis, band, edge_opacity| Some((axis, FocusBand { band, edge_opacity, blur: 0.0 }));
    let cases = [
        (roll, None, 0, 3, 1.0, 1.0, None),
        (EdgeFade::NONE, band(RampAxis::Vertical, 0.3, 0.1), 0, -2, 0.8, 1.0, None),
        (lower_third, band(RampAxis::Horizontal, 0.5, 0.4), 4, 0, 1.0, 1.0, None),
        (all, None, 0, 0, 0.6, 1.2, None),
        (roll, band(RampAxis::Vertical, 0.5, 0.4), 1, 2, 1.0, 1.0, Some([20, 24, 40, 255])),
    ];
    for (edges, focus, dx, dy, opacity, scale, under) in cases {
        let fade = ScreenFade::new(edges, focus).unwrap();
        let rows = (0, H);
        let anchor = (W as f32 / 2.0, H as f32 / 2.0);
        let r = raster(&rgba);
        let want_stack = klipp_title::pose_sprite(&r, W, H, dx, dy, opacity, scale, anchor, under, Some(fade), rows);
        assert!(gpu.stack_begin(W, H, 1));
        assert!(upload_stack(&mut gpu, &want_stack));
        assert!(gpu.stack_blend(0, 1.0, 0));
        assert!(gpu.stack_finish_into_chain(true));
        let want = gpu.stack_download_linear();

        let sprite_shape = klipp_effects::gpu::SpriteShape {
            top: edges.top,
            bottom: edges.bottom,
            left: edges.left,
            right: edges.right,
            focus_vertical: focus.map(|(axis, _)| axis == RampAxis::Vertical),
            band: focus.map_or(0.5, |(_, f)| f.band),
            edge_opacity: focus.map_or(1.0, |(_, f)| f.edge_opacity),
        };
        assert!(gpu.stack_begin(W, H, 1));
        assert!(gpu.stack_layer_place_sprite_posed(0, 93, &r.texels, sprite_box(&r), dx, dy, opacity, scale, anchor, under, Some(sprite_shape), rows.0, rows.1));
        assert!(gpu.stack_blend(0, 1.0, 0));
        assert!(gpu.stack_finish_into_chain(true));
        let got = gpu.stack_download_linear();
        // One smoothstep on both sides, unrounded on both (#190).
        let worst = worst_level(&want, &got);
        assert!(worst <= 0.5, "faded sprite ({edges:?}, {focus:?}, {dx}, {dy}, {opacity}, ×{scale}, {under:?}) differs by {worst:.3} levels");
        if under.is_none() {
            // And it shows: an edge that fades is fainter than the middle.
            let alpha = |x: u32, y: u32| got[((y * W + x) * 4 + 3) as usize];
            let middle = alpha(W / 2, H / 2);
            let across = edges.left > 0.0 || matches!(focus, Some((RampAxis::Horizontal, _)));
            let edge = if across { alpha(0, H / 2) } else { alpha(W / 2, 0) };
            assert!(edge < middle, "{edges:?} {focus:?}: edge {edge} is fainter than the middle {middle}");
        }
    }
}

/// A focus blur on a title layer (#592) — words on transparency — is the
/// CPU's to a level, alpha included: soft outside the band, the words
/// as they are inside it.
#[test]
fn a_focus_blur_on_a_layer_is_the_cpu_one() {
    let Some(mut gpu) = pipeline() else { return };
    // "Words": bars of opaque colour on a transparent layer, top to bottom.
    let mut layer = vec![0f32; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            if y % 8 < 3 && (8..W - 8).contains(&x) && x % 5 != 0 {
                let i = ((y * W + x) * 4) as usize;
                layer[i..i + 4].copy_from_slice(&[0.9, 0.8, 0.2, 1.0]);
            }
        }
    }
    for (vertical, band) in [(true, 0.3), (false, 0.2)] {
        let fx = [klipp_effects::BuiltinEffect::FocusBlur { radius: 3.0, vertical, band }];
        let want = klipp_effects::apply_effects(&fx, layer.clone(), W, H);
        assert!(gpu.stack_begin(W, H, 1));
        // As the stack runs a layer's effects: a plan over the layer, the
        // result back in its slot.
        gpu.upload_chain_input_linear(&layer);
        gpu.begin_chain();
        let run = gpu.chain_run_effects(&fx, true, W, H);
        gpu.submit_chain();
        assert!(run.complete);
        assert!(gpu.stack_layer_from_chain(0, run.in_a));
        assert!(gpu.stack_blend(0, 1.0, 0));
        assert!(gpu.stack_finish_into_chain(true));
        let got = gpu.stack_download_linear();
        let cpu_stack = {
            assert!(gpu.stack_begin(W, H, 1));
            assert!(gpu.stack_layer_from_linear(0, &want));
            assert!(gpu.stack_blend(0, 1.0, 0));
            assert!(gpu.stack_finish_into_chain(true));
            gpu.stack_download_linear()
        };
        let worst = worst_level(&cpu_stack, &got);
        assert!(worst <= 1.0, "focus blur (vertical {vertical}, band {band}) differs by {worst:.3} levels");
        // Premultiplied: white words soften white, not grey, though the
        // colour under their transparency is black (#598 review).
        for px in want.chunks_exact(4).filter(|p| p[3] > 0.05 && p[3] < 0.95) {
            assert!((px[0] - 0.9).abs() < 0.02 && (px[1] - 0.8).abs() < 0.02, "vertical {vertical}: a soft edge keeps its colour: {px:?}");
        }
        // Soft at the edge of the axis, sharp in the middle: alpha between
        // the bars is lifted there and not here.
        let a = |x: u32, y: u32| want[((y * W + x) * 4 + 3) as usize];
        // Gaps: the rows between bars going down, the columns (x % 5 == 0)
        // between their pieces going across.
        let (edge_gap, middle_gap) = if vertical { (a(W / 2, 4), a(W / 2, H / 2 - H / 2 % 8 + 4)) } else { (a(10, 1), a(W / 2 - W / 2 % 5, 1)) };
        assert!(edge_gap > middle_gap + 0.05, "vertical {vertical}: the edge is soft ({edge_gap}) where the middle is not ({middle_gap})");
    }
}

/// A sprite's glyphs placed one by one on the GPU — each moved, scaled
/// about its own box and faded, laid over the ones before — are
/// `klipp_title::pose_sprite_glyphs` decoded as rows, to a level.
#[test]
fn posed_glyphs_are_the_cpu_glyph_pose_uploaded() {
    let Some(mut gpu) = pipeline() else { return };
    // Three "glyphs": boxes of solid colour with soft edges, and a
    // halo round each that the map gives to the nearest box.
    let boxes = [[4i32, 8, 14, 30], [18, 8, 30, 30], [34, 10, 44, 28]];
    let colours = [[240u8, 60, 40], [40, 220, 90], [70, 90, 250]];
    let mut rgba = vec![0u8; (W * H * 4) as usize];
    let (ux0, uy0, uw, uh) = (0u32, 4u32, W, 32u32);
    let mut index = vec![u16::MAX; (uw * uh) as usize];
    for y in 0..H as i32 {
        for x in 0..W as i32 {
            let mut best = (f32::MAX, u16::MAX);
            for (g, b) in boxes.iter().enumerate() {
                let ddx = (b[0] as f32 - x as f32).max(x as f32 - b[2] as f32).max(0.0);
                let ddy = (b[1] as f32 - y as f32).max(y as f32 - b[3] as f32).max(0.0);
                let d = (ddx * ddx + ddy * ddy).sqrt();
                if d < best.0 {
                    best = (d, g as u16);
                }
            }
            let a = ((3.0 - best.0) / 3.0 * 255.0).clamp(0.0, 255.0) as u8;
            if a > 0 {
                let i = ((y as u32 * W + x as u32) * 4) as usize;
                let c = colours[best.1 as usize];
                rgba[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], a]);
                if y as u32 >= uy0 && (y as u32) < uy0 + uh {
                    index[((y as u32 - uy0) * uw + x as u32) as usize] = best.1;
                }
            }
        }
    }
    let units = klipp_title::UnitMap { x0: ux0, y0: uy0, w: uw, h: uh, index, boxes: boxes.to_vec(), parents: vec!['a', 'b', 'c'], pad: 3, path: None, tracking_origins: Vec::new() };
    let pose = |g: usize, dx: f32, dy: f32, scale: f32, alpha: f32, clip: Option<(i32, i32)>| {
        let b = boxes[g];
        let anchor = ((b[0] + b[2]) as f32 * 0.5, (b[1] + b[3]) as f32 * 0.5);
        let grow = |v: i32, c: f32, sign: f32| c + (v as f32 + sign * 3.0 - c) * scale;
        let mut moved = [
            (grow(b[0], anchor.0, -1.0) + dx).floor() as i32 - 1,
            (grow(b[1], anchor.1, -1.0) + dy).floor() as i32 - 1,
            (grow(b[2], anchor.0, 1.0) + dx).ceil() as i32 + 1,
            (grow(b[3], anchor.1, 1.0) + dy).ceil() as i32 + 1,
        ];
        if let Some((y0, y1)) = clip {
            moved[1] = moved[1].max(y0);
            moved[3] = moved[3].min(y1);
        }
        klipp_title::GlyphPose { dx, dy, scale, alpha, anchor, clip, moved, rotation: 0.0 }
    };
    // A glyph an animator turns (#1510): its moved box round the turned
    // corners, as `glyph_poses` makes it.
    let turned = |g: usize, rotation: f32, scale: f32, dx: f32| {
        let b = boxes[g];
        let anchor = ((b[0] + b[2]) as f32 * 0.5, (b[1] + b[3]) as f32 * 0.5);
        let (c, sn) = (rotation.cos(), rotation.sin());
        let (hx, hy) = ((b[2] - b[0]) as f32 * 0.5 + 3.0, (b[3] - b[1]) as f32 * 0.5 + 3.0);
        let (ex, ey) = ((hx * c).abs() + (hy * sn).abs(), (hx * sn).abs() + (hy * c).abs());
        let moved = [
            (anchor.0 - ex * scale + dx).floor() as i32 - 1,
            (anchor.1 - ey * scale).floor() as i32 - 1,
            (anchor.0 + ex * scale + dx).ceil() as i32 + 1,
            (anchor.1 + ey * scale).ceil() as i32 + 1,
        ];
        klipp_title::GlyphPose { dx, dy: 0.0, scale, alpha: 1.0, anchor, clip: None, moved, rotation }
    };
    let cases: Vec<Vec<klipp_title::GlyphPose>> = vec![
        vec![pose(0, 0.0, 0.0, 1.0, 1.0, None), pose(1, 0.0, 0.0, 1.0, 1.0, None), pose(2, 0.0, 0.0, 1.0, 1.0, None)],
        vec![pose(0, 5.0, -3.0, 1.0, 0.6, None), pose(1, 0.0, 6.0, 1.0, 1.0, None), pose(2, -8.0, 0.0, 1.0, 0.3, None)],
        vec![pose(0, 0.0, 0.0, 1.3, 1.0, None), pose(1, 2.0, 1.0, 0.7, 0.8, None), pose(2, 0.0, 0.0, 1.0, 0.0, None)],
        vec![pose(0, 0.0, 9.0, 1.0, 1.0, Some((8, 30))), pose(1, 0.0, 4.0, 1.0, 1.0, Some((8, 30))), pose(2, 0.0, 0.0, 1.0, 1.0, None)],
        // Glyphs slid over one another: the later one lies on top.
        vec![pose(0, 14.0, 0.0, 1.0, 1.0, None), pose(1, 0.0, 0.0, 1.0, 0.7, None), pose(2, -16.0, 0.0, 1.0, 1.0, None)],
        // Turned by animators, one scaled and moved too.
        vec![turned(0, 0.6, 1.0, 0.0), turned(1, -1.2, 1.4, 3.0), turned(2, std::f32::consts::FRAC_PI_2, 0.8, -2.0)],
    ];
    for (n, poses) in cases.iter().enumerate() {
        let r = raster(&rgba);
        let want_stack = klipp_title::pose_sprite_glyphs(&r, W, H, &units, poses, (0, H));
        assert!(gpu.stack_begin(W, H, 1));
        assert!(upload_stack(&mut gpu, &want_stack));
        assert!(gpu.stack_blend(0, 1.0, 0));
        assert!(gpu.stack_finish_into_chain(true));
        let want = gpu.stack_download_linear();

        let packed: Vec<[f32; 16]> = poses.iter().map(|p| p.packed()).collect();
        assert!(gpu.stack_begin(W, H, 1));
        assert!(gpu.stack_layer_place_glyphs(0, 93, &r.texels, sprite_box(&r), (units.x0, units.y0, units.w, units.h, &units.index), &packed, 0, H));
        assert!(gpu.stack_blend(0, 1.0, 0));
        assert!(gpu.stack_finish_into_chain(true));
        let got = gpu.stack_download_linear();
        let worst = worst_level(&want, &got);
        let at = want.iter().zip(&got).enumerate().max_by(|a, b| (a.1 .0 - a.1 .1).abs().total_cmp(&(b.1 .0 - b.1 .1).abs())).map(|(i, _)| i).unwrap_or(0);
        let px = at / 4;
        assert!(
            worst <= 0.5,
            "posed glyphs case {n} differ by {worst:.3} levels at pixel ({}, {}) channel {}: twin {:?} gpu {:?}, twin stack {:?}",
            px % W as usize,
            px / W as usize,
            at % 4,
            &want[px * 4..px * 4 + 4],
            &got[px * 4..px * 4 + 4],
            &want_stack[px * 4..px * 4 + 4],
        );
    }
}
