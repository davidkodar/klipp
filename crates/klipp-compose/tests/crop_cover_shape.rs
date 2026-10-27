//! A crop to a shape (#2757): two pictures, one a 16:9 source and one a
//! 9:16 one, each set to cover half of a wide, a tall and a square frame
//! by a Crop given the half's shape and a Transform to the half. Each
//! half is covered to its corners by its own picture and nothing of
//! either reaches the other half; the GPU stack draws it with no
//! fallback, and as the CPU does. A missing GPU is reported as
//! unverified; with `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame};
use klipp_core::{MediaId, RationalTime, Timecode};
use klipp_effects::BuiltinEffect;
use klipp_timeline::{Clip, Sequence, Track, TrackKind};

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// The first media a wide red picture, the second a tall blue one.
struct Shaped(MediaId);

impl FrameSource for Shaped {
    fn frame(&mut self, clip: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        let (c, w, h) = if clip.media_id() == Some(self.0) { ([0.8f32, 0.05, 0.05, 1.0], 320, 180) } else { ([0.05f32, 0.1, 0.8, 1.0], 180, 320) };
        Some(SourceFrame { pixels: klipp_compose::SourcePixels::Linear(std::sync::Arc::new(c.repeat(w * h))), width: w as u32, height: h as u32, pts: None })
    }
}

/// A media clip covering the box `(x, y, w, h)` (pixels) of a `fw` × `fh`
/// frame: cropped to the box's shape, then placed on it.
fn covering(media: MediaId, fw: f32, fh: f32, b: (f32, f32, f32, f32)) -> Clip {
    let mut c = Clip::media(media, tc(0.0), tc(3.0), tc(0.0), tc(3.0));
    let a = b.2 / b.3;
    c.push_effect(BuiltinEffect::Crop { left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0, aspect: a, focus_x: 0.5, focus_y: 0.5 });
    // The cover is the largest rectangle of the shape in the frame.
    let rw = if a > fw / fh { fw } else { fh * a };
    let s = b.2 / rw;
    c.push_effect(BuiltinEffect::Transform {
        position_x: b.0 + b.2 / 2.0 - fw / 2.0,
        position_y: b.1 + b.3 / 2.0 - fh / 2.0,
        scale_x: s,
        scale_y: s,
        rotation: 0.0,
        anchor_x: 0.5,
        anchor_y: 0.5,
        rotation_x: 0.0,
        rotation_y: 0.0,
        perspective: 0.0,
        focal_ref: None,
    });
    c
}

#[test]
fn a_crop_to_a_shape_covers_its_box_from_any_source() {
    let (wide, tall) = (MediaId::new(), MediaId::new());
    for (w, h) in [(384u32, 216u32), (216, 384), (240, 240)] {
        let (fw, fh) = (w as f32, h as f32);
        // Halves side by side, or one above the other on a tall frame.
        let (a, b) = if h > w { ((0.0, 0.0, fw, fh / 2.0), (0.0, fh / 2.0, fw, fh / 2.0)) } else { ((0.0, 0.0, fw / 2.0, fh), (fw / 2.0, 0.0, fw / 2.0, fh)) };
        for (first, second) in [(wide, tall), (tall, wide)] {
            let mut v1 = Track::new("V1", TrackKind::Video);
            v1.clips = vec![covering(first, fw, fh, a)];
            let mut v2 = Track::new("V2", TrackKind::Video);
            v2.clips = vec![covering(second, fw, fh, b)];
            let mut seq = Sequence::new_default();
            seq.resolution = klipp_core::Resolution { width: w, height: h };
            seq.tracks = vec![v2, v1];
            let red = |p: [u8; 3]| p[0] > 150 && p[2] < 100;
            let blue = |p: [u8; 3]| p[2] > 150 && p[0] < 100;
            let (in_a, in_b): (&dyn Fn([u8; 3]) -> bool, &dyn Fn([u8; 3]) -> bool) = if first == wide { (&red, &blue) } else { (&blue, &red) };
            let check = |px: &[u8], how: &str| {
                let at = |x: f32, y: f32| { let i = ((y as u32 * w + x as u32) * 4) as usize; [px[i], px[i + 1], px[i + 2]] };
                // Each box's corners, three pixels in, and its middle.
                for (bx, ours) in [(a, in_a), (b, in_b)] {
                    for (x, y) in [(bx.0 + 3.0, bx.1 + 3.0), (bx.0 + bx.2 - 4.0, bx.1 + 3.0), (bx.0 + 3.0, bx.1 + bx.3 - 4.0), (bx.0 + bx.2 - 4.0, bx.1 + bx.3 - 4.0), (bx.0 + bx.2 / 2.0, bx.1 + bx.3 / 2.0)] {
                        assert!(ours(at(x, y)), "{how} {w}x{h}: ({x}, {y}) of its box shows {:?}", at(x, y));
                    }
                }
            };
            let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            cpu.set_gpu_enabled(false);
            let c = klipp_effects::linear_f32_rgba_to_srgb_u8(&cpu.render(&seq, tc(1.0), w, h, &mut Shaped(wide), RenderOptions::default()));
            check(&c, "CPU");
            let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            gpu.set_gpu_enabled(true);
            let Some((g, _)) = gpu.render_display_u8_on_gpu(&seq, tc(1.0), w, h, &mut Shaped(wide), RenderOptions::default()) else {
                assert!(!required(), "required, but the GPU stack declined the cover");
                eprintln!("UNVERIFIED: no GPU stack");
                return;
            };
            assert_eq!(klipp_effects::residency::last_frame().fallback_count(), 0, "on the GPU");
            check(&g, "GPU");
            let worst = g.iter().zip(&c).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0);
            assert!(worst <= 3, "{w}x{h}: GPU and CPU differ by {worst} levels");
        }
    }
}
