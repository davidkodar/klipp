//! A title reaches the GPU stack as the box it covers in half floats
//! (#190), by each of its three routes: a resting render placed, its
//! glyphs posed one by one, or a frame rendered for its time. Each lands
//! within one sRGB level of the frame the byte path renders, the CPU's
//! `render_with_window` uploaded as bytes.

use klipp_core::{Interpolation, Keyframe};
use klipp_effects::gpu::{GpuEffectsPipeline, SpriteBox};
use klipp_timeline::{LayerFx, Motion, MotionKind, StaggerUnit, TextBox, TitleData, TitleMotion};
use klipp_title::{BoxRaster, TitleFrame, TitleRenderer};

const W: u32 = 640;
const H: u32 = 360;

fn sprite_box(r: &BoxRaster) -> SpriteBox {
    SpriteBox { x0: r.x0, y0: r.y0, w: r.w, h: r.h }
}

/// A lower third: two lines low on the frame.
fn lower_third(fx: LayerFx) -> TitleData {
    let mut td = TitleData::single_text_fx(TextBox { text: "Ana Ruiz\nDirector".into(), font_size: 48.0, ..Default::default() }, fx);
    td.objects[0].transform.center = klipp_timeline::TitlePosition { x: 0.3, y: 0.82 };
    td
}

/// What the stack makes of layer 0, read back: straight linear RGBA.
fn read(gpu: &mut GpuEffectsPipeline, layer: impl FnOnce(&mut GpuEffectsPipeline) -> bool) -> Vec<f32> {
    assert!(gpu.stack_begin(W, H, 1));
    assert!(layer(gpu), "the layer goes up");
    assert!(gpu.stack_blend(0, 1.0, 0));
    assert!(gpu.stack_finish_into_chain(true));
    gpu.stack_download_linear()
}

fn to_code(linear: f32) -> f32 {
    let c = linear.clamp(0.0, 1.0);
    255.0 * if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// The worst difference in sRGB codes, premultiplied, so colour under no
/// alpha counts for nothing.
fn worst_code(a: &[f32], b: &[f32]) -> f32 {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .map(|(p, q)| {
            let alpha = (p[3] - q[3]).abs() * 255.0;
            let colour = (0..3).map(|k| (to_code(p[k]) * p[3] - to_code(q[k]) * q[3]).abs()).fold(0.0, f32::max);
            alpha.max(colour)
        })
        .fold(0.0, f32::max)
}

#[test]
fn each_route_lands_within_a_level_of_the_byte_frame() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    let key = |time_secs, value| Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) };
    let mut keyed = lower_third(LayerFx::default());
    if let klipp_timeline::TitleObjectContent::TextBox(tb) = &mut keyed.objects[0].content {
        tb.font_size_keyframes = vec![key(0.0, 40.0), key(2.0, 80.0)];
    }
    let cases: [(&str, TitleData, f32); 5] = [
        ("still", lower_third(LayerFx::default()), 2.0),
        ("sliding in", lower_third(LayerFx { motion: TitleMotion::enter(Motion::new(MotionKind::Rise).duration(1.0)), ..Default::default() }), 0.4),
        ("scaling up", lower_third(LayerFx { motion: TitleMotion::enter(Motion::new(MotionKind::ScaleUp).duration(1.0)), ..Default::default() }), 0.4),
        ("letter by letter", lower_third(LayerFx { motion: TitleMotion::enter(Motion::new(MotionKind::Rise).duration(1.0).stagger(StaggerUnit::Char, 0.05)), ..Default::default() }), 0.4),
        ("keyed size", keyed, 1.0),
    ];
    let mut routes = Vec::new();
    for (name, td, t) in cases {
        let bytes = TitleRenderer::new().render_with_window(&td, W, H, None, t, Some(4.0));
        let want = read(&mut gpu, |g| g.stack_layer_from_srgb_u8(0, &bytes));
        let mut r = TitleRenderer::new();
        let frame = r.frame_with_window(&td, W, H, None, t, Some(4.0));
        let (route, got) = match frame {
            TitleFrame::Placed { sprite, dx, dy, opacity, scale, anchor, under, shape, rows, blur } => {
                assert!(blur <= 0.5 && shape.is_none(), "{name}: a plain placement");
                let got = read(&mut gpu, |g| g.stack_layer_place_sprite_posed(0, 1, &sprite.pixels.texels, sprite_box(&sprite.pixels), dx, dy, opacity, scale, anchor, under, None, rows.0, rows.1));
                ("placed", got)
            }
            TitleFrame::Glyphs { sprite, poses, rows } => {
                let u = sprite.units.clone().expect("a glyph map");
                let packed: Vec<[f32; 16]> = poses.iter().map(|p| p.packed()).collect();
                let got = read(&mut gpu, |g| g.stack_layer_place_glyphs(0, 2, &sprite.pixels.texels, sprite_box(&sprite.pixels), (u.x0, u.y0, u.w, u.h, &u.index), &packed, rows.0, rows.1));
                ("glyphs", got)
            }
            TitleFrame::Rendered { pixels, .. } => {
                let got = read(&mut gpu, |g| g.stack_layer_from_srgb_f16_box(0, &pixels.texels, sprite_box(&pixels)));
                ("rendered", got)
            }
        };
        let worst = worst_code(&want, &got);
        assert!(worst <= 1.0, "{name} by the {route} route: {worst:.3} sRGB levels from the byte frame");
        routes.push(route);
    }
    for route in ["placed", "glyphs", "rendered"] {
        assert!(routes.contains(&route), "the cases take every route: {routes:?}");
    }
}

/// A lower third's resting raster holds only its box, in less than the
/// frame took as bytes, and gives the frame back as the byte path drew it.
#[test]
fn a_lower_third_holds_less_than_its_frame_as_bytes() {
    let (w, h) = (1920u32, 1080u32);
    let td = lower_third(LayerFx::default());
    let mut r = TitleRenderer::new();
    let TitleFrame::Placed { sprite, .. } = r.frame_with_window(&td, w, h, None, 2.0, Some(4.0)) else {
        panic!("a still lower third is its resting render placed");
    };
    let bytes = (w * h * 4) as usize;
    assert!(sprite.pixels.byte_size() < bytes, "{} bytes against {bytes} as a byte frame", sprite.pixels.byte_size());
    assert!(sprite.pixels.byte_size() * 4 < bytes, "a lower third is far smaller than its frame: {} bytes", sprite.pixels.byte_size());
    let frame = TitleRenderer::new().render_with_window(&td, w, h, None, 2.0, Some(4.0));
    assert_eq!(sprite.pixels.to_bytes(), frame, "nothing outside the box is lost");
}

/// GPU placement and the static reference agree at several tracking values,
/// including a backwards seek; the same uploaded sprite is retained.
#[test]
fn tracking_glyphs_match_static_reference_on_gpu() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "GPU required");
        return;
    };
    let key = |time_secs, value| Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) };
    let td = TitleData::single_text(TextBox { text: "A little space".into(), font_size: 60.0, tracking: 2.0,
        tracking_keyframes: vec![key(0.0, 2.0), key(2.0, 14.0)], ..Default::default() });
    let mut r = TitleRenderer::new();
    let mut id = None;
    for t in [0.0, 0.5, 2.0, 1.0, 0.0] {
        let mut still = td.clone();
        let tb = still.primary_text_mut().unwrap();
        *tb = tb.at(t as f64);
        let bytes = TitleRenderer::new().render_with_window(&still, W, H, None, t, Some(4.0));
        let want = read(&mut gpu, |g| g.stack_layer_from_srgb_u8(0, &bytes));
        let TitleFrame::Glyphs { sprite, poses, rows } = r.frame_with_window(&td, W, H, None, t, Some(4.0)) else { panic!("tracking uses glyph placement") };
        assert_eq!(*id.get_or_insert(sprite.id), sprite.id);
        let u = sprite.units.as_ref().unwrap();
        let packed: Vec<_> = poses.iter().map(|p| p.packed()).collect();
        let got = read(&mut gpu, |g| g.stack_layer_place_glyphs(0, 72, &sprite.pixels.texels, sprite_box(&sprite.pixels), (u.x0, u.y0, u.w, u.h, &u.index), &packed, rows.0, rows.1));
        assert!(worst_code(&want, &got) <= 1.0, "t={t}: {}", worst_code(&want, &got));
    }
}

/// A repeatable 4K measurement through title evaluation, upload, GPU placement
/// and composition. Wait for GPU completion, rather than timing submission.
/// This deliberately excludes decode, UI presentation and audio.
#[test]
#[ignore = "4K timing comparison; run alone with --ignored --nocapture"]
fn tracking_4k_cached_and_raster_composition_timing() {
    use std::time::Instant;
    let mut gpu = GpuEffectsPipeline::new().expect("timing requires GPU");
    let key = |time_secs, value| Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) };
    let td = TitleData::single_text_fx(TextBox { text: "Give it a little space.".into(), font_name: "Inter".into(),
        font_style: klipp_timeline::FontStyle::Bold, font_size: 90.0,
        tracking_keyframes: vec![key(0.0, 0.0), key(6.0, 10.0)], ..Default::default() },
        LayerFx { motion: TitleMotion::both(Motion::new(MotionKind::MaskRise).duration(0.9).stagger(StaggerUnit::Word, 0.1),
            Motion::new(MotionKind::Fade).duration(0.6)), ..Default::default() });
    for cached in [false, true] {
        let mut td = td.clone();
        if !cached {
            // Constant size keys force the pre-optimization raster route without
            // changing a pixel of the intended animation.
            td.primary_text_mut().unwrap().font_size_keyframes = vec![key(0.0, 90.0)];
        }
        let mut renderer = TitleRenderer::new();
        let mut samples = Vec::new();
        let mut sprite_id = None;
        let before = gpu.readback_stats();
        for f in 0..61 {
            let start = Instant::now();
            let frame = renderer.frame_with_window(&td, 3840, 2160, None, f as f32 / 10.0, Some(7.0));
            assert!(gpu.stack_begin(3840, 2160, 1));
            match frame {
                TitleFrame::Glyphs { sprite, poses, rows } if cached => {
                    assert_eq!(*sprite_id.get_or_insert(sprite.id), sprite.id);
                    let u = sprite.units.as_ref().unwrap();
                    let packed: Vec<_> = poses.iter().map(|p| p.packed()).collect();
                    assert!(gpu.stack_layer_place_glyphs(0, 7272, &sprite.pixels.texels, sprite_box(&sprite.pixels),
                        (u.x0, u.y0, u.w, u.h, &u.index), &packed, rows.0, rows.1));
                }
                TitleFrame::Rendered { pixels, .. } if !cached => {
                    assert!(gpu.stack_layer_from_srgb_f16_box(0, &pixels.texels, sprite_box(&pixels)));
                }
                _ => panic!("unexpected route; cached={cached}"),
            }
            assert!(gpu.stack_blend(0, 1.0, 0));
            assert!(gpu.stack_finish_into_chain(true));
            gpu.stack_end();
            gpu.device().poll(wgpu::Maintain::Wait);
            if f > 0 { samples.push(start.elapsed().as_secs_f64() * 1000.0); }
        }
        assert_eq!(before, gpu.readback_stats(), "composition stays on GPU");
        samples.sort_by(f64::total_cmp);
        println!("4K tracking cached={cached}: median {:.3} ms, p95 {:.3} ms", samples[30], samples[57]);
    }
}
