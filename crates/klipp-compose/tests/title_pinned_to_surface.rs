//! A title pinned to a tracked surface (#2751) is drawn on the surface,
//! following it, on the GPU stack and on the CPU path; with its text
//! changed after pinning, the new text is drawn there too. A missing GPU
//! is reported as unverified; with `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame};
use klipp_core::{MediaId, RationalTime, Timecode};
use klipp_effects::BuiltinEffect as E;
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, ClipKind, Sequence, Surface, SurfaceKey, SurfacePin, Track, TrackKind};

const W: u32 = 320;
const H: u32 = 180;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

struct Nothing;

impl FrameSource for Nothing {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        None
    }
}

/// A surface a quarter of the frame across, at `x` from the left.
fn quad(x: f32) -> [[f32; 2]; 4] {
    [[x, 0.3], [x + 0.25, 0.3], [x + 0.25, 0.6], [x, 0.6]]
}

/// A host clip tracked from x 0.1 at file 0 s to x 0.6 at 4 s, and white
/// `text` pinned to that surface on the track above.
fn scene(text: &str) -> Sequence {
    scene_losing(text, None, Default::default())
}

/// [`scene`], the track lost from `lost_from` file seconds on (the keys
/// there holding the last good place), the pin doing `on_loss` there.
fn scene_losing(text: &str, lost_from: Option<f64>, on_loss: klipp_timeline::PinLoss) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut host = Clip::media(MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0));
    let id = klipp_core::ClipId::new().0;
    host.surfaces.push(Surface {
        id,
        name: "Wall".into(),
        corners: quad(0.1),
        reference_secs: 0.0,
        keys: match lost_from {
            None => vec![SurfaceKey::at_corners(0.0, &quad(0.1), &quad(0.1)).unwrap(), SurfaceKey::at_corners(4.0, &quad(0.1), &quad(0.6)).unwrap()],
            // A key every half second, lost from `from` on, held there.
            Some(from) => (0..=8)
                .map(|k| {
                    let t = k as f64 * 0.5;
                    let x = 0.1 + 0.125 * t.min(from) as f32;
                    let key = SurfaceKey::at_corners(t, &quad(0.1), &quad(x)).unwrap();
                    if t >= from { SurfaceKey { state: klipp_timeline::TrackState::Lost, confidence: Some(0.0), ..key } } else { key }
                })
                .collect(),
        },
        source_size: [W, H],
        media_fps: 0.0,
        track_area: None,
        exclude: Vec::new(),
    });
    let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: text.into(), font_size: 300.0, ..Default::default() });
    let [tl, tr, br, bl] = quad(0.1);
    let mut title = TestClip::of(Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(4.0)) }).effect(E::CornerPin { tl, tr, br, bl }).build();
    title.surface_pin = Some(SurfacePin { host: host.id, surface: id, picture_size: None, fit: Default::default(), inset: 0.0, on_loss, light: 0.0 });
    let mut video = Track::new("V1", TrackKind::Video);
    video.clips = vec![host];
    let mut words = Track::new("T1", TrackKind::Text);
    words.clips = vec![title];
    seq.tracks = vec![words, video];
    seq
}

/// The brightest red level in the frame.
fn peak(px: &[u8]) -> u8 {
    px.chunks_exact(4).map(|p| p[0]).max().unwrap_or(0)
}

/// The frame at `secs`, on the CPU path and, when there is one, the GPU
/// stack, which must agree.
fn frame(seq: &Sequence, secs: f64) -> Vec<u8> {
    let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    cpu.set_gpu_enabled(false);
    let want = klipp_effects::linear_f32_rgba_to_srgb_u8(&cpu.render(seq, tc(secs), W, H, &mut Nothing, RenderOptions::default()));
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    match r.render_display_u8_on_gpu(seq, tc(secs), W, H, &mut Nothing, RenderOptions::default()) {
        Some((stack, _)) => {
            let worst = stack.iter().zip(&want).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
            assert!(worst <= 3, "at {secs} s the stack differs from the CPU path by {worst} levels");
        }
        None => assert!(!required(), "no GPU stack"),
    }
    want
}

/// The box round every lit pixel: x0, y0, x1, y1, in fractions.
fn lit(px: &[u8]) -> [f32; 4] {
    let mut b = (W, H, 0, 0);
    for y in 0..H {
        for x in 0..W {
            if px[((y * W + x) * 4) as usize] > 128 {
                b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
            }
        }
    }
    assert!(b.0 <= b.2, "the title is drawn");
    [b.0 as f32 / W as f32, b.1 as f32 / H as f32, (b.2 + 1) as f32 / W as f32, (b.3 + 1) as f32 / H as f32]
}

/// The title's lit box at `secs` on the CPU path and, when there is one,
/// the GPU stack, which must agree.
fn drawn(seq: &Sequence, secs: f64) -> [f32; 4] {
    let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    cpu.set_gpu_enabled(false);
    let want = klipp_effects::linear_f32_rgba_to_srgb_u8(&cpu.render(seq, tc(secs), W, H, &mut Nothing, RenderOptions::default()));
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    match r.render_display_u8_on_gpu(seq, tc(secs), W, H, &mut Nothing, RenderOptions::default()) {
        Some((stack, _)) => {
            let worst = stack.iter().zip(&want).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
            assert!(worst <= 3, "at {secs} s the stack differs from the CPU path by {worst} levels");
        }
        None => {
            assert!(!required(), "no GPU stack");
            eprintln!("GPU stack unverified: no GPU");
        }
    }
    lit(&want)
}

/// Inside the surface's quad, `margin` of slack for the glyphs' soft edges.
fn within(b: [f32; 4], q: [[f32; 2]; 4], margin: f32) -> bool {
    b[0] >= q[0][0] - margin && b[1] >= q[0][1] - margin && b[2] <= q[2][0] + margin && b[3] <= q[2][1] + margin
}

#[test]
fn a_pinned_title_is_drawn_on_its_surface_and_follows_it() {
    let seq = scene("Klipp");
    let start = drawn(&seq, 0.0);
    assert!(within(start, quad(0.1), 0.01), "on the surface at the start: {start:?}");
    // Halfway through the track the surface is at x 0.35.
    let half = drawn(&seq, 2.0);
    assert!(within(half, quad(0.35), 0.01), "following it: {half:?}");
    assert!((half[0] - start[0] - 0.25).abs() < 0.02, "moved with the surface: {start:?} -> {half:?}");
}

#[test]
fn a_pinned_titles_new_text_is_drawn_on_its_surface() {
    let before = drawn(&scene("Hi"), 2.0);
    let after = drawn(&scene("Hello there"), 2.0);
    assert!(within(after, quad(0.35), 0.01), "the new text on the surface: {after:?}");
    assert!(before != after, "the new text is what is drawn: {before:?} vs {after:?}");
}

/// #2751: where the surface's track is lost, a pin set to hide is gone,
/// one set to fade dims over its fade, and one set to hold (the default)
/// stays at the last good place, on the GPU stack as on the CPU path.
#[test]
fn a_pinned_title_hides_fades_or_holds_where_its_track_is_lost() {
    use klipp_timeline::PinLoss;
    let hold = scene_losing("Klipp", Some(2.0), PinLoss::Hold);
    let held = peak(&frame(&hold, 3.0));
    assert!(held > 200, "held: still drawn ({held})");
    assert!(within(lit(&frame(&hold, 3.0)), quad(0.35), 0.01), "at the last good place");
    let hide = scene_losing("Klipp", Some(2.0), PinLoss::Hide);
    assert!(peak(&frame(&hide, 1.5)) > 200, "followed: drawn");
    assert!(peak(&frame(&hide, 3.0)) < 8, "lost: hidden ({})", peak(&frame(&hide, 3.0)));
    let fade = scene_losing("Klipp", Some(2.0), PinLoss::Fade { secs: 1.0 });
    let (at_loss, half, after) = (peak(&frame(&fade, 1.9)), peak(&frame(&fade, 2.5)), peak(&frame(&fade, 3.5)));
    assert!(at_loss > 200 && half > 40 && half < at_loss - 30 && after < 8, "fading: {at_loss} then {half} then {after}");
}

/// #2751: a pinned title taking its surface's light is darker where the
/// surface is darker than on its reference frame, by the light measured
/// on each frame, on the GPU stack as on the CPU path; one not taking it
/// stays as it is.
#[test]
fn a_pinned_title_taking_the_light_darkens_with_its_surface() {
    let lit = |light: f32| {
        let mut seq = scene("Klipp");
        // The surface half as light (as encoded) from 2 s on.
        let host = seq.tracks[1].clips.first_mut().unwrap();
        let s = &mut host.surfaces[0];
        let (from, to) = (s.keys[0], s.keys[1]);
        s.keys = (0..=8)
            .map(|k| {
                let t = k as f64 * 0.5;
                let base = if t < 4.0 { from } else { to };
                SurfaceKey { source_secs: t, light: Some(if t < 2.0 { 0.8 } else { 0.4 }), ..base }
            })
            .collect();
        let pinned = seq.tracks[0].clips.first_mut().unwrap();
        pinned.surface_pin.as_mut().unwrap().light = light;
        seq
    };
    let taking = lit(1.0);
    let (bright, dark) = (peak(&frame(&taking, 1.0)) as u32, peak(&frame(&taking, 3.0)) as u32);
    assert!(dark < bright * 3 / 4 && dark > bright / 5, "darker with its surface: {bright} then {dark}");
    let not = lit(0.0);
    let (a, b) = (peak(&frame(&not, 1.0)), peak(&frame(&not, 3.0)));
    assert!(a.abs_diff(b) <= 2, "not taking it: {a} and {b}");
}

